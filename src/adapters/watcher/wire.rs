//! Length-delimited frames. Query strings are UTF-8, the caller's cwd is Unix bytes.
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    os::unix::ffi::OsStringExt,
    path::PathBuf,
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

pub(super) fn reply(out: &mut impl Write, code: u8, bytes: &[u8]) -> io::Result<()> {
    out.write_all(&[code])?;
    out.write_all(&(bytes.len() as u64).to_le_bytes())?;
    out.write_all(bytes)
}
