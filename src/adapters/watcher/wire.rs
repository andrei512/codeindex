//! Length-delimited frames. Query strings are UTF-8, the caller's cwd is Unix bytes.
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

const MAX_REQUEST: usize = 2 * 1024 * 1024;

pub(super) fn send_field(out: &mut impl Write, field: &[u8]) -> io::Result<()> {
    let len = u32::try_from(field.len()).map_err(io::Error::other)?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(field)
}

fn read_field(input: &mut impl Read, budget: &mut usize) -> io::Result<Vec<u8>> {
    let mut len = [0; 4];
    input.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    *budget = budget
        .checked_sub(len)
        .ok_or_else(|| io::Error::other("oversized cx request"))?;
    let mut field = vec![0; len];
    input.read_exact(&mut field)?;
    Ok(field)
}

pub(super) struct Query {
    pub context: Vec<u8>,
    pub cwd: PathBuf,
    pub args: Vec<String>,
}

pub(super) fn write_query(
    out: &mut impl Write,
    context: &[u8],
    cwd: &Path,
    args: &[String],
) -> io::Result<()> {
    let mut send = io::BufWriter::new(out);
    send_field(&mut send, context)?;
    send_field(&mut send, cwd.as_os_str().as_bytes())?;
    send.write_all(&[args.len() as u8])?;
    for arg in args {
        send_field(&mut send, arg.as_bytes())?;
    }
    send.flush()
}
pub(super) enum Response {
    Diagnostic(u8),
    Fallback,
    Reelect,
    Output(u8),
    InvalidArgs,
}
impl Response {
    fn decode(code: u8) -> Self {
        match code {
            0..=2 => Self::Diagnostic(code),
            4 => Self::Reelect,
            5..=7 => Self::Output(code - 5),
            8 => Self::InvalidArgs,
            _ => Self::Fallback,
        }
    }
    fn encode(self) -> u8 {
        match self {
            Self::Diagnostic(code) => code,
            Self::Fallback => 3,
            Self::Reelect => 4,
            Self::Output(code) => 5 + code,
            Self::InvalidArgs => 8,
        }
    }
}

pub(super) fn response_header(input: &mut impl Read) -> io::Result<(Response, u64)> {
    let mut header = [0; 9];
    input.read_exact(&mut header)?;
    Ok((
        Response::decode(header[0]),
        u64::from_le_bytes(header[1..].try_into().unwrap()),
    ))
}

pub(super) fn read_query(input: &mut impl Read) -> io::Result<Query> {
    let mut budget = MAX_REQUEST;
    let context = read_field(input, &mut budget)?;
    let cwd = OsString::from_vec(read_field(input, &mut budget)?).into();
    let mut count = [0; 1];
    input.read_exact(&mut count)?;
    if !(1..=8).contains(&count[0]) {
        return Err(io::Error::other("invalid cx request length"));
    }
    let args = (0..count[0])
        .map(|_| String::from_utf8(read_field(input, &mut budget)?).map_err(io::Error::other))
        .collect::<io::Result<_>>()?;
    Ok(Query { context, cwd, args })
}

pub(super) fn reply(out: &mut impl Write, code: Response, bytes: &[u8]) -> io::Result<()> {
    out.write_all(&[code.encode()])?;
    out.write_all(&(bytes.len() as u64).to_le_bytes())?;
    out.write_all(bytes)
}
pub(super) fn write_output(
    out: &mut impl Write,
    exit: u8,
    text: &[u8],
    diagnostic: &[u8],
) -> io::Result<()> {
    reply(out, Response::Output(exit), text)?;
    out.write_all(&(diagnostic.len() as u64).to_le_bytes())?;
    out.write_all(diagnostic)
}
pub(super) fn read_output(
    input: &mut impl Read,
    len: u64,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<()> {
    copy_frame(input, len, out)?;
    let mut length = [0; 8];
    input.read_exact(&mut length)?;
    copy_frame(input, u64::from_le_bytes(length), err)
}
pub(super) fn copy_frame(input: &mut impl Read, len: u64, out: &mut impl Write) -> io::Result<()> {
    if io::copy(&mut input.take(len), out)? != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete cx watcher response",
        ));
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_round_trip_and_short_diagnostics() {
        let mut response = Vec::new();
        write_output(&mut response, 1, b"match", b"warning").unwrap();
        assert_eq!(response[0], 6);
        assert_eq!(read_match(&response).unwrap(), b"warning");
        let short = &response[..response.len() - 1];
        assert_eq!(read_match(short).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert!(matches!(Response::decode(255), Response::Fallback));
    }

    fn read_match(mut input: &[u8]) -> io::Result<Vec<u8>> {
        let (code, length) = response_header(&mut input).unwrap();
        assert!(matches!(code, Response::Output(1)));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let result = read_output(&mut input, length, &mut stdout, &mut stderr);
        assert_eq!(stdout, b"match");
        result.map(|()| stderr)
    }
}
