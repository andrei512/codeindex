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

fn open_relative(root: &File, path: &Path) -> io::Result<File> {
    #[cfg(target_os = "macos")]
    {
        // Darwin's O_NOFOLLOW_ANY checks every component in one name lookup.
        const NOFOLLOW_ANY: i32 = 0x20000000;
        let name = CString::new(path.as_os_str().as_bytes()).map_err(invalid)?;
        let (_, _, cloexec, nonblock) = FLAGS;
        let fd = unsafe {
            openat(
                root.as_raw_fd(),
                name.as_ptr(),
                NOFOLLOW_ANY | cloexec | nonblock,
                0 as c_int,
            )
        };
        if fd >= 0 {
            return Ok(unsafe { File::from_raw_fd(fd) });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::InvalidInput {
            return Err(error);
        }
    }
    open_components(root, path)
}

fn open_components(root: &File, path: &Path) -> io::Result<File> {
    let (nofollow, directory, cloexec, nonblock) = FLAGS;
    let mut file = None::<File>;
    let mut parts = path.components().peekable();
    while let Some(component) = parts.next() {
        let Component::Normal(name) = component else {
            return Err(invalid("unsafe repository path"));
        };
        let name = CString::new(name.as_bytes()).map_err(invalid)?;
        let options = nofollow
            | cloexec
            | if parts.peek().is_some() {
                directory
            } else {
                nonblock
            };
        // Each component is relative to the already-open directory and rejects symlinks.
        let parent = file.as_ref().unwrap_or(root);
        let fd = unsafe { openat(parent.as_raw_fd(), name.as_ptr(), options, 0 as c_int) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        file = Some(unsafe { File::from_raw_fd(fd) });
    }
    file.ok_or_else(|| invalid("empty path"))
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

fn read_text(mut input: impl Read, len: u64) -> io::Result<Option<String>> {
    // Read into the allocation retained by the snapshot builder. Take caps
    // each read before a binary prefix can reserve an entire large file.
    // Leave space for read_to_end's final EOF check on short regular files.
    let mut all = Vec::with_capacity((len.min(BLOCK as u64) as usize) + 1);
    let mut good = 0;
    loop {
        let previous = all.len();
        let n = input.by_ref().take(BLOCK as u64).read_to_end(&mut all)?;
        if all[previous..].contains(&0) {
            return Ok(None);
        }
        match std::str::from_utf8(&all[good..]) {
            Ok(_) => good = all.len(),
            Err(e) if e.error_len().is_none() => good += e.valid_up_to(),
            Err(_) => return Ok(None),
        }
        if n < BLOCK {
            break;
        }
        let reservation = len.min(8 * 1024 * 1024) as usize;
        if good > 0 && all.capacity() < reservation {
            all.reserve(reservation - all.len());
        }
    }
    if good != all.len() {
        return Ok(None);
    }
    // The incremental checks validated the entire contiguous buffer.
    Ok(Some(unsafe { String::from_utf8_unchecked(all) }))
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
fn stable_read<T>(
    root: &Path,
    path: &Path,
    expected: [u64; 7],
    read: impl FnOnce(&File) -> io::Result<T>,
) -> io::Result<T> {
    let file = open_relative(&File::open(root)?, path)?;
    stable_read_open(root, path, expected, &file, read)
}

fn stable_read_open<T>(
    root: &Path,
    path: &Path,
    expected: [u64; 7],
    file: &File,
    read: impl FnOnce(&File) -> io::Result<T>,
) -> io::Result<T> {
    let changed = || io::Error::other("repository file changed during capture");
    let opened = file.metadata()?;
    if !opened.is_file() || stamp(&opened) != expected {
        return Err(changed());
    }
    let result = read(file)?;
    if stamp(&file.metadata()?) != expected
        || stamp(&std::fs::symlink_metadata(root.join(path))?) != expected
    {
        return Err(changed());
    }
    Ok(result)
}

pub(super) fn read_control(root: &Path, path: &Path, metadata: &Metadata) -> io::Result<Vec<u8>> {
    stable_read(root, path, stamp(metadata), |mut file| {
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
        let path = checked_path(path)?;
        match open_relative(&self.dir, path) {
            Ok(file) => stable_read_open(self.root, path, expected, &file, |file| {
                read_text(file, expected[2])
            }),
            Err(error) => {
                // Observed symlinks intentionally have path rows but no source.
                let current = std::fs::symlink_metadata(self.root.join(path))?;
                if current.is_symlink() && stamp(&current) == expected {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
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
    stable_read(root, path, expected, |file| read_text(file, before.len()))
}
pub fn capture(root: &Path, path: &str) -> io::Result<Option<String>> {
    let path = checked_path(path)?;
    let before = std::fs::symlink_metadata(root.join(path))?;
    if !before.is_file() {
        return Ok(None);
    }
    stable_read(root, path, stamp(&before), |file| {
        read_text(file, before.len())
    })
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
