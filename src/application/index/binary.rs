use std::io::{self, Write};

pub fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid native index data")
}
pub fn put(w: &mut impl Write, n: usize) -> io::Result<()> {
    w.write_all(&(n as u64).to_le_bytes())
}
pub fn string(w: &mut impl Write, s: &str) -> io::Result<()> {
    put(w, s.len())?;
    w.write_all(s.as_bytes())
}

pub struct Decoder<'a>(pub &'a [u8]);
impl<'a> Decoder<'a> {
    pub fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let (a, b) = self.0.split_at_checked(n).ok_or_else(bad)?;
        self.0 = b;
        Ok(a)
    }
    pub fn num(&mut self) -> io::Result<usize> {
        usize::try_from(u64::from_le_bytes(self.take(8)?.try_into().unwrap())).map_err(|_| bad())
    }
    pub fn string(&mut self) -> io::Result<&'a str> {
        let n = self.num()?;
        std::str::from_utf8(self.take(n)?).map_err(|_| bad())
    }
    pub fn end(&self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(bad())
        }
    }
}
