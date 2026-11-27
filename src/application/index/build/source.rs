//! Captures exceeding the retained heap allowance share one anonymous scratch file.
//! Offsets are private build handles; publication still emits the existing SOURCE codec.
use std::{
    fs::{self, File, OpenOptions},
    io,
    ops::Range,
    os::unix::{
        fs::{FileExt, OpenOptionsExt},
        io::AsRawFd,
    },
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

pub(super) enum Source {
    Heap(String),
    Scratch(Span),
}
pub(super) struct Span {
    range: Range<usize>,
    file: Arc<File>,
}
pub(super) struct Pool {
    retained: AtomicUsize,
    limit: usize,
    scratch: Mutex<Option<Arc<File>>>,
    length: AtomicUsize,
}
impl Pool {
    pub fn new(limit: usize) -> Self {
        Self {
            retained: AtomicUsize::new(0),
            limit,
            scratch: Mutex::new(None),
            length: AtomicUsize::new(0),
        }
    }
    pub fn retain(&self, body: String) -> io::Result<Source> {
        let cap = body.capacity();
        if body.is_empty() {
            return Ok(Source::Heap(String::new()));
        }
        if self
            .retained
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                n.checked_add(cap).filter(|&v| v <= self.limit)
            })
            .is_ok()
        {
            return Ok(Source::Heap(body));
        }
        let start = self
            .length
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                n.checked_add(body.len())
                    .filter(|&v| v <= isize::MAX as usize)
            })
            .map_err(|_| io::Error::other("source scratch size overflow"))?;
        let fd = {
            let mut scratch = self
                .scratch
                .lock()
                .map_err(|_| io::Error::other("source scratch lock poisoned"))?;
            if scratch.is_none() {
                *scratch = Some(Arc::new(anonymous_file()?))
            }
            Arc::clone(scratch.as_ref().unwrap())
        };
        fd.write_all_at(body.as_bytes(), start as u64)?;
        Ok(Source::Scratch(Span {
            range: start..start + body.len(),
            file: fd,
        }))
    }
    // Ownership prevents further writes before the mapping is shared with readers.
    pub fn seal(self) -> io::Result<Option<Mapping>> {
        let n = self.length.into_inner();
        let scratch = self
            .scratch
            .into_inner()
            .map_err(|_| io::Error::other("source scratch lock poisoned"))?;
        scratch.map(|f| Mapping::open(f, n)).transpose()
    }
}
fn anonymous_file() -> io::Result<File> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = std::env::temp_dir();
    loop {
        let path = tmp.join(format!(
            ".codeindex-source-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(f) => {
                fs::remove_file(path)?;
                return Ok(f);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}
unsafe extern "C" {
    fn mmap(
        address: *mut std::ffi::c_void,
        len: usize,
        protection: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    fn munmap(address: *mut std::ffi::c_void, len: usize) -> i32;
}
pub(super) struct Mapping {
    file: Arc<File>,
    address: NonNull<u8>,
    len: usize,
}
// The only descriptor was private to Pool; sealing joins writers and consumes it.
// The read-only mapping remains valid until the last borrowed slice is dropped.
unsafe impl Sync for Mapping {}
impl Mapping {
    fn open(file: Arc<File>, len: usize) -> io::Result<Self> {
        if len == 0 || file.metadata()?.len() != len as u64 {
            return Err(io::Error::other("source scratch length mismatch"));
        }
        // PROT_READ, MAP_PRIVATE are equal on the supported Darwin/Linux hosts.
        let p = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0) };
        if p as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        let address =
            NonNull::new(p.cast()).ok_or_else(|| io::Error::other("null source mapping"))?;
        Ok(Self { address, len, file })
    }
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.address.as_ptr(), self.len) }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            munmap(self.address.as_ptr().cast(), self.len);
        }
    }
}
impl Source {
    pub fn text<'a>(&'a self, mapping: Option<&'a Mapping>) -> &'a str {
        match self {
            Self::Heap(text) => text,
            Self::Scratch(span) => {
                let mapping = mapping.expect("sealed source scratch");
                assert!(Arc::ptr_eq(&span.file, &mapping.file));
                let bytes = &mapping.bytes()[span.range.clone()];
                // Pool wrote exactly this String to an exclusive, non-overlapping range.
                unsafe { std::str::from_utf8_unchecked(bytes) }
            }
        }
    }
}

// Reserve one share of the machine's memory for source. Other builders currently
// account independently. Kernel-reclaimable mapped pages are outside the heap cap.
pub(super) fn source_allowance() -> usize {
    available_memory().map_or(64 << 20, |bytes| (bytes / 64).max(1) as usize)
}

#[cfg(target_os = "macos")]
fn available_memory() -> Option<u64> {
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            old: *mut std::ffi::c_void,
            len: *mut usize,
            new: *mut std::ffi::c_void,
            newlen: usize,
        ) -> i32;
    }
    let mut bytes = 0u64;
    let mut len = 8usize;
    let status = unsafe {
        sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut bytes as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && len == 8 && bytes > 0).then_some(bytes)
}

#[cfg(target_os = "linux")]
fn available_memory() -> Option<u64> {
    let info = fs::read_to_string("/proc/meminfo").ok()?;
    let kib = info.lines().find_map(|line| {
        line.strip_prefix("MemAvailable:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    let bytes = kib.saturating_mul(1024);
    Some(cgroup_free_memory().map_or(bytes, |free| bytes.min(free)))
}

#[cfg(target_os = "linux")]
fn cgroup_free_memory() -> Option<u64> {
    let max = fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
    let used = fs::read_to_string("/sys/fs/cgroup/memory.current").ok()?;
    let max = max.trim().parse::<u64>().ok()?;
    let used = used.trim().parse::<u64>().ok()?;
    Some(max.saturating_sub(used))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn available_memory() -> Option<u64> {
    None
}
