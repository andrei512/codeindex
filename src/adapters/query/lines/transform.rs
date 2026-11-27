use super::super::Glob;
use std::{fs::File, io, path::Path, process::Command};

#[derive(Default)]
pub(super) struct Transform<'a> {
    pub compressed: bool,
    pub program: Option<&'a str>,
    globs: Vec<(bool, Glob)>,
    has_include: bool,
}
impl<'a> Transform<'a> {
    pub fn add_glob(&mut self, value: &str) -> Result<(), String> {
        let pattern = value.strip_prefix('!').unwrap_or(value);
        let include = pattern == value;
        self.globs.push((include, Glob::new(pattern)?));
        self.has_include |= include;
        Ok(())
    }
    pub fn active(&self) -> bool {
        self.compressed || self.program.is_some()
    }
    fn decoder(&self, path: &Path) -> Option<&'static [&'static str]> {
        if !self.compressed {
            return None;
        }
        const DECODERS: &[(&[&str], &[&str])] = &[
            (&["gz", "tgz"], &["gzip", "-dc"]),
            (&["bz2", "tbz2"], &["bzip2", "-dc"]),
            (&["xz", "txz"], &["xz", "-dc"]),
            (&["lzma"], &["xz", "--format=lzma", "-dc"]),
            (&["br"], &["brotli", "-dc"]),
            (&["zst", "zstd"], &["zstd", "-qdc"]),
            (&["lz4"], &["lz4", "-dc"]),
            (&["Z"], &["uncompress", "-c"]),
        ];
        let extension = path.extension()?.to_str()?;
        DECODERS.iter().find_map(|(extensions, command)| extensions.contains(&extension).then_some(*command))
    }
    pub fn reader(&self, name: &str) -> Option<Reader<'a>> {
        if let Some(program) = self.program {
            let base = name.rsplit('/').next().unwrap_or(name);
            return self
                .globs
                .iter()
                .rev()
                .find(|(_, g)| g.is_match(name) || g.is_match(base))
                .map_or(!self.has_include, |(include, _)| *include)
                .then_some(Reader::Program(program));
        }
        self.decoder(Path::new(name)).map(Reader::Decoder)
    }
}

pub(super) enum Reader<'a> {
    Program(&'a str),
    Decoder(&'static [&'static str]),
}

impl Reader<'_> {
    pub fn read(&self, path: &Path, cwd: &Path) -> io::Result<Vec<u8>> {
        let mut command = match self {
            Self::Program(program) => {
                let mut c = Command::new(program);
                c.arg(path);
                c
            }
            Self::Decoder(decoder) => {
                let mut c = Command::new(decoder[0]);
                c.args(&decoder[1..]);
                c
            }
        };
        let output = command.current_dir(cwd).stdin(File::open(path)?).output();
        if let Err(e) = &output
            && matches!(self, Self::Decoder(_))
            && e.kind() == io::ErrorKind::NotFound
        {
            return std::fs::read(path);
        }
        let output = output?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }
}
