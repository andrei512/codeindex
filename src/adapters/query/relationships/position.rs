//! Public relationship positions use byte columns. The structured
//! navigation interface uses one-based UTF-16 code units.
use super::{Pin, io};

#[derive(Clone, Copy)]
pub(super) struct Position {
    line: usize,
    col: usize,
    encoding: Encoding,
}

#[derive(Clone, Copy)]
pub(super) enum Encoding {
    Bytes,
    Utf16,
}

impl Position {
    pub(super) fn parse(line: &str, col: &str, encoding: Encoding) -> Result<Self, String> {
        let positive = |text: &str| {
            text.parse::<usize>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| "expected a positive position".to_owned())
        };
        Ok(Self {
            line: positive(line)?,
            col: positive(col)?,
            encoding,
        })
    }

    pub(super) fn offset(self, pin: &Pin, file: usize) -> io::Result<usize> {
        let Self {
            line,
            col,
            encoding,
        } = self;
        let entry = &pin.files[file];
        let start = entry
            .line_start(line)
            .ok_or_else(|| io::Error::other("line outside document"))?;
        let end = entry
            .line_ends
            .get(line - 1)
            .map(|&n| n as u64)
            .unwrap_or_else(|| entry.bytes.as_ref().map_or(0, |r| r.end - r.start));
        let text = pin.text_range(file, start as u64..end)?;
        let column = match encoding {
            Encoding::Bytes => byte_offset(&text, col)?,
            Encoding::Utf16 => utf16_offset(text.split('\n').next().unwrap(), col)?,
        };
        Ok(start + column)
    }
}

fn byte_offset(line: &str, col: usize) -> io::Result<usize> {
    let offset = col - 1;
    let body = line.trim_end_matches(['\r', '\n']);
    if offset >= body.len() || !body.is_char_boundary(offset) {
        return Err(io::Error::other("invalid byte column"));
    }
    Ok(offset)
}

fn utf16_offset(line: &str, col: usize) -> io::Result<usize> {
    let mut units = 1;
    let mut offset = 0;
    for c in line.chars() {
        if units >= col {
            break;
        }
        units += c.len_utf16();
        offset += c.len_utf8();
    }
    if units != col {
        return Err(io::Error::other(
            "position outside line or inside surrogate pair",
        ));
    }
    Ok(offset)
}
