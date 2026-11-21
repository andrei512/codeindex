//! An FSEvents callback invalidates the speculative worktree observation.
//! The request path always takes a new observation, even after a native flush.
use super::Wake;
use std::{
    ffi::{CString, c_char, c_void},
    os::unix::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::mpsc,
};

type Ref = *mut c_void;
#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Ref,
    release: Ref,
    describe: Ref,
}
type Callback = unsafe extern "C" fn(Ref, *mut c_void, usize, Ref, *const u32, *const u64);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: Ref,
        callback: Callback,
        ctx: *mut Context,
        paths: Ref,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> Ref;
    fn FSEventStreamScheduleWithRunLoop(stream: Ref, run_loop: Ref, mode: Ref);
    fn FSEventStreamStart(stream: Ref) -> u8;
    fn FSEventStreamStop(stream: Ref);
    fn FSEventStreamInvalidate(stream: Ref);
    fn FSEventStreamRelease(stream: Ref);
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithFileSystemRepresentation(allocator: Ref, bytes: *const c_char) -> Ref;
    fn CFArrayCreate(allocator: Ref, values: *const Ref, count: isize, callbacks: Ref) -> Ref;
    fn CFRelease(value: Ref);
    fn CFRunLoopGetCurrent() -> Ref;
    fn CFRunLoopRun();
    static kCFRunLoopDefaultMode: Ref;
}
unsafe extern "C" fn changed(
    _: Ref,
    context: *mut c_void,
    _: usize,
    _: Ref,
    _: *const u32,
    _: *const u64,
) {
    // The boxed context stays on this same thread until the stream is invalidated.
    unsafe { &*(context as *const Wake) }.dirty();
}

pub(super) fn start(root: &Path, wake: Wake) -> bool {
    let Ok(path) = CString::new(root.as_os_str().as_bytes()) else {
        return false;
    };
    let (ready, started) = mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("cx-fsevents".into())
        .spawn(move || unsafe {
            let null = ptr::null_mut();
            let path = CFStringCreateWithFileSystemRepresentation(null, path.as_ptr());
            if path.is_null() {
                return;
            }
            // The array borrows the string. Both outlive the stream.
            let paths = CFArrayCreate(null, &path, 1, null);
            if paths.is_null() {
                CFRelease(path);
                return;
            }
            let mut wake = Box::new(wake);
            let mut ctx = Context {
                version: 0,
                info: (&mut *wake as *mut Wake).cast(),
                retain: null,
                release: null,
                describe: null,
            };
            // SinceNow; WatchRoot | IgnoreSelf. All notifications (including loss)
            // widen to a single full-reconcile bit, so callback storage stays bounded.
            let stream =
                FSEventStreamCreate(null, changed, &mut ctx, paths, u64::MAX, 0.15, 0x04 | 0x08);
            if !stream.is_null() {
                FSEventStreamScheduleWithRunLoop(
                    stream,
                    CFRunLoopGetCurrent(),
                    kCFRunLoopDefaultMode,
                );
                if FSEventStreamStart(stream) != 0 {
                    let _ = ready.send(true);
                    CFRunLoopRun();
                    FSEventStreamStop(stream);
                }
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
            }
            CFRelease(paths);
            CFRelease(path);
        });
    thread.is_ok()
        && started
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap_or(false)
}
