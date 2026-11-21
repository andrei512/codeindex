//! Component driver. Input contains (id, UTF-8 byte length, body) frames.
//! The output is one raw content extent, suitable for independent comparison.
use codeindex::domain::{Document, FileId, content::ContentBuilder};
use std::io::{self, BufRead, BufWriter, Read, Write};

fn main() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut builder = ContentBuilder::default();
    let mut body = Vec::new();
    while !input.fill_buf()?.is_empty() {
        let mut header = [0; 16];
        input.read_exact(&mut header)?;
        let id = u64::from_le_bytes(header[..8].try_into().unwrap());
        let size = usize::try_from(u64::from_le_bytes(header[8..].try_into().unwrap()))
            .map_err(io::Error::other)?;
        body.resize(size, 0);
        input.read_exact(&mut body)?;
        let text = std::str::from_utf8(&body).map_err(io::Error::other)?;
        builder.add(Document {
            file: FileId(id),
            text,
        })?;
    }
    let mut out = BufWriter::new(io::stdout().lock());
    builder.seal(&mut out)?;
    out.flush()
}
