use super::invalid;
use std::{
    ffi::{CString, c_char, c_int},
    fs::{File, Metadata},
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

#[cfg(target_os = "macos")]
const FLAGS: (i32, i32, i32, i32) = (0x100, 0x100000, 0x1000000, 4);
#[cfg(any(target_os = "linux", target_os = "android"))]
const FLAGS: (i32, i32, i32, i32) = if cfg!(any(target_arch = "arm", target_arch = "aarch64")) {
    (0o100000, 0o40000, 0o2000000, 0o4000)
} else {
    (0o400000, 0o200000, 0o2000000, 0o4000)
};
unsafe extern "C" {
    fn openat(fd: c_int, path: *const c_char, options: c_int, ...) -> c_int;
}

fn open_at(parent: &File, name: &CString, flags: c_int) -> io::Result<File> {
    let fd = unsafe { openat(parent.as_raw_fd(), name.as_ptr(), flags, 0 as c_int) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_relative(root: &File, path: &Path) -> io::Result<File> {
    #[cfg(target_os = "macos")]
    {
        // Darwin's O_NOFOLLOW_ANY checks every component in one name lookup.
        const NOFOLLOW_ANY: i32 = 0x20000000;
        let name = CString::new(path.as_os_str().as_bytes()).map_err(invalid)?;
        let (_, _, cloexec, nonblock) = FLAGS;
        match open_at(root, &name, NOFOLLOW_ANY | cloexec | nonblock) {
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
            result => return result,
        }
    }
    open_components(root, path)
}

fn open_components(root: &File, path: &Path) -> io::Result<File> {
    let mut file = None::<File>;
    let mut parts = path.components().peekable();
    while let Some(component) = parts.next() {
        let name = component_name(component)?;
        // Each component is relative to the already-open directory and rejects symlinks.
        let parent = file.as_ref().unwrap_or(root);
        file = Some(open_at(parent, &name, component_flags(parts.peek().is_some()))?);
    }
    file.ok_or_else(|| invalid("empty path"))
}
fn component_flags(has_child: bool) -> i32 {
    let (nofollow, directory, cloexec, nonblock) = FLAGS;
    let mode = if has_child { directory } else { nonblock };
    nofollow | cloexec | mode
}

fn component_name(component: Component<'_>) -> io::Result<CString> {
    match component {
        Component::Normal(name) => CString::new(name.as_bytes()).map_err(invalid),
        _ => Err(invalid("unsafe repository path")),
    }
}

pub(super) fn stamp(m: &Metadata) -> [u64; 7] {
    [
        m.dev(),
        m.ino(),
        m.size(),
        m.mtime() as u64,
        m.mtime_nsec() as u64,
        m.ctime() as u64,
        m.ctime_nsec() as u64,
    ]
}
const BLOCK: usize = 64 * 1024;

struct TextRead {
    all: Vec<u8>,
    good: usize,
    reservation: usize,
}

impl TextRead {
    fn new(len: u64) -> Self {
        // Extra capacity lets read_to_end check EOF without growing short files.
        Self {
            all: Vec::with_capacity((len.min(BLOCK as u64) as usize) + 1),
            good: 0,
            reservation: len.min(8 * 1024 * 1024) as usize,
        }
    }

    fn read(&mut self, input: &mut impl Read) -> io::Result<Option<usize>> {
        let previous = self.all.len();
        let n = input.take(BLOCK as u64).read_to_end(&mut self.all)?;
        if self.all[previous..].contains(&0) {
            return Ok(None);
        }
        match std::str::from_utf8(&self.all[self.good..]) {
            Ok(_) => self.good = self.all.len(),
            Err(error) if error.error_len().is_none() => self.good += error.valid_up_to(),
            Err(_) => return Ok(None),
        }
        Ok(Some(n))
    }

    fn reserve(&mut self) {
        if self.good > 0 && self.all.capacity() < self.reservation {
            self.all.reserve(self.reservation - self.all.len());
        }
    }

    fn finish(self) -> Option<String> {
        if self.good != self.all.len() {
            return None;
        }
        // The incremental checks validated the entire contiguous buffer.
        Some(unsafe { String::from_utf8_unchecked(self.all) })
    }
}

fn read_text(mut input: impl Read, len: u64) -> io::Result<Option<String>> {
    let mut text = TextRead::new(len);
    while let Some(n) = text.read(&mut input)? {
        if n < BLOCK {
            return Ok(text.finish());
        }
        text.reserve();
    }
    Ok(None)
}

fn checked_path(path: &str) -> io::Result<&Path> {
    let relative = Path::new(path);
    if path.is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid("expected a repository-relative path"));
    }
    Ok(relative)
}
struct FileObservation<'a> {
    root: &'a Path,
    path: &'a Path,
    expected: [u64; 7],
}

impl FileObservation<'_> {
    fn read<T>(&self, decode: impl FnOnce(&File) -> io::Result<T>) -> io::Result<T> {
        let file = open_relative(&File::open(self.root)?, self.path)?;
        self.read_open(&file, decode)
    }

    fn read_open<T>(&self, file: &File, decode: impl FnOnce(&File) -> io::Result<T>) -> io::Result<T> {
        let opened = file.metadata()?;
        if !opened.is_file() || !self.matches(&opened) {
            return Err(capture_changed());
        }
        let result = decode(file)?;
        if !self.matches(&file.metadata()?) || !self.matches(&self.at_path()?) {
            return Err(capture_changed());
        }
        Ok(result)
    }

    fn at_path(&self) -> io::Result<Metadata> {
        std::fs::symlink_metadata(self.root.join(self.path))
    }

    fn matches(&self, metadata: &Metadata) -> bool {
        stamp(metadata) == self.expected
    }

    fn absent_source<T>(&self, error: io::Error) -> io::Result<Option<T>> {
        // Observed symlinks intentionally have path rows but no source.
        let current = self.at_path()?;
        if current.is_symlink() && self.matches(&current) {
            Ok(None)
        } else {
            Err(error)
        }
    }
}

fn capture_changed() -> io::Error {
    io::Error::other("repository file changed during capture")
}

pub(super) fn read_control(root: &Path, path: &Path, metadata: &Metadata) -> io::Result<Vec<u8>> {
    let observation = FileObservation { root, path, expected: stamp(metadata) };
    observation.read(|mut file| {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}
pub(crate) struct CaptureRoot<'a> {
    root: &'a Path,
    dir: File,
}

impl<'a> CaptureRoot<'a> {
    pub fn new(root: &'a Path) -> io::Result<Self> {
        Ok(Self {
            root,
            dir: File::open(root)?,
        })
    }

    pub fn capture(&self, path: &str, expected: [u64; 7]) -> io::Result<Option<String>> {
        self.read(path, expected, |file| read_text(file, expected[2]))
    }

    pub fn bytes(&self, path: &str, expected: [u64; 7]) -> io::Result<Option<Vec<u8>>> {
        self.read(path, expected, |mut file| {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        })
    }

    fn read<T>(
        &self,
        path: &str,
        expected: [u64; 7],
        reader: impl FnOnce(&File) -> io::Result<Option<T>>,
    ) -> io::Result<Option<T>> {
        let path = checked_path(path)?;
        let observation = FileObservation { root: self.root, path, expected };
        match open_relative(&self.dir, path) {
            Ok(file) => observation.read_open(&file, reader),
            Err(error) => observation.absent_source(error),
        }
    }
}

pub fn capture_observed(root: &Path, path: &str, expected: [u64; 7]) -> io::Result<Option<String>> {
    let path = checked_path(path)?;
    let before = std::fs::symlink_metadata(root.join(path))?;
    if stamp(&before) != expected {
        return Err(io::Error::other("repository file changed since discovery"));
    }
    if !before.is_file() {
        return Ok(None);
    }
    FileObservation { root, path, expected }.read(|file| read_text(file, before.len()))
}
pub fn capture(root: &Path, path: &str) -> io::Result<Option<String>> {
    let path = checked_path(path)?;
    let before = std::fs::symlink_metadata(root.join(path))?;
    if !before.is_file() {
        return Ok(None);
    }
    FileObservation { root, path, expected: stamp(&before) }
        .read(|file| read_text(file, before.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_utf8_and_nul() {
        let mut data = vec![b'a'; BLOCK - 1];
        data.extend_from_slice("🌍".as_bytes());
        assert_eq!(
            read_text(&data[..], data.len() as u64)
                .unwrap()
                .unwrap()
                .len(),
            data.len()
        );
        data.pop();
        assert!(read_text(&data[..], 0).unwrap().is_none());
        assert!(read_text(&b"\0abc"[..], 1 << 40).unwrap().is_none());
    }
}
