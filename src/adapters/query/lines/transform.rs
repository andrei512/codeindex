use super::super::Glob;
use std::{fs::File, io, path::Path, process::Command};

#[derive(Default)]
pub(super) struct Transform<'a> {
    pub compressed: bool,
    pub program: Option<&'a str>,
    pub globs: Vec<(bool, Glob)>,
}
impl Transform<'_> {
    pub fn active(&self) -> bool {
        self.compressed || self.program.is_some()
    }
    fn decoder(&self, path: &Path) -> Option<&'static [&'static str]> {
        if !self.compressed {
            return None;
        }
        Some(match path.extension()?.to_str()? {
            "gz" | "tgz" => &["gzip", "-dc"],
            "bz2" | "tbz2" => &["bzip2", "-dc"],
            "xz" | "txz" => &["xz", "-dc"],
            "lzma" => &["xz", "--format=lzma", "-dc"],
            "br" => &["brotli", "-dc"],
            "zst" | "zstd" => &["zstd", "-qdc"],
            "lz4" => &["lz4", "-dc"],
            "Z" => &["uncompress", "-c"],
            _ => return None,
        })
    }
    pub fn applies(&self, name: &str) -> bool {
        if self.program.is_some() {
            let base = name.rsplit('/').next().unwrap_or(name);
            return self
                .globs
                .iter()
                .rev()
                .find(|(_, g)| g.is_match(name) || g.is_match(base))
                .map_or(
                    !self.globs.iter().any(|(include, _)| *include),
                    |(include, _)| *include,
                );
        }
        self.decoder(Path::new(name)).is_some()
    }
    pub fn read(&self, path: &Path, cwd: &Path) -> io::Result<Vec<u8>> {
        let mut command = if let Some(program) = self.program {
            let mut c = Command::new(program);
            c.arg(path);
            c
        } else {
            let decoder = self.decoder(path).expect("selected compressed filename");
            let mut c = Command::new(decoder[0]);
            c.args(&decoder[1..]);
            c
        };
        let output = command.current_dir(cwd).stdin(File::open(path)?).output();
        if let Err(e) = &output
            && self.program.is_none()
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
