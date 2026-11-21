//! One speculative writer per worktree. Requests on a live owner reconcile
//! through the same Pin::refresh entry point as a standalone invocation.
#[cfg(target_os = "macos")]
mod native;
mod wire;
use super::{
    query::{Request, evaluate},
    repository,
    snapshot::Pin,
};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};

pub(crate) const INTERNAL_ARG: &str = "--internal-watch";
const IDLE: Duration = Duration::from_secs(5 * 60);
const FALLBACK: u8 = 3;
const REELECT: u8 = 4;
const PROTOCOL: &[u8] = b"CXW1";

struct IdleDeadline(Instant);
impl IdleDeadline {
    fn from_request(admitted: Instant) -> Self {
        Self(admitted + IDLE)
    }
    fn remaining(&self, now: Instant) -> Option<Duration> {
        let wait = self.0.saturating_duration_since(now);
        (!wait.is_zero()).then_some(wait)
    }
}

struct Endpoint {
    cache: PathBuf,
    socket: PathBuf,
    lock: PathBuf,
}

impl Endpoint {
    fn new(root: &Path) -> io::Result<Self> {
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        let uid = unsafe { geteuid() };
        // Darwin's sockaddr_un path is only 104 bytes. A user-private short
        // runtime directory also works for arbitrarily long HOME/worktree paths.
        let runtime = PathBuf::from(format!("/tmp/cx-{uid}"));
        match fs::DirBuilder::new().mode(0o700).create(&runtime) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
        let meta = fs::symlink_metadata(&runtime)?;
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
            return Err(io::Error::other("unsafe cx watcher directory"));
        }
        let home = env::var_os("HOME").ok_or_else(|| io::Error::other("missing HOME"))?;
        let cache = fs::canonicalize(home)?
            .join(".codeindex/repos")
            .join(repository::cache_key(root));
        let key = repository::cache_key(&cache);
        Ok(Self {
            cache,
            socket: runtime.join(&key),
            lock: runtime.join(format!("{key}.lock")),
        })
    }
}

fn executable_identity(path: &Path) -> io::Result<[u8; 32]> {
    let exe = fs::metadata(path)?;
    let mut identity = [0; 32];
    for (field, value) in identity.chunks_exact_mut(8).zip([
        exe.dev(),
        exe.ino(),
        exe.ctime() as u64,
        exe.ctime_nsec() as u64,
    ]) {
        field.copy_from_slice(&value.to_le_bytes());
    }
    Ok(identity)
}

fn context(executable: &[u8; 32]) -> io::Result<Vec<u8>> {
    let mut key = Vec::from(PROTOCOL);
    key.extend_from_slice(executable);
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
    ] {
        match env::var_os(name) {
            None => key.push(0),
            Some(v) => {
                if name != "GIT_CONFIG_NOSYSTEM" && !Path::new(&v).is_absolute() {
                    return Err(io::Error::other(
                        "relative Git configuration requires caller's cwd",
                    ));
                }
                key.push(1);
                wire::send_field(&mut key, v.as_bytes())?;
            }
        }
    }
    Ok(key)
}

fn connect(root: &Path, endpoint: &Endpoint) -> io::Result<UnixStream> {
    if let Ok(conn) = UnixStream::connect(&endpoint.socket) {
        return Ok(conn);
    }
    let mut cmd = Command::new(env::current_exe()?);
    cmd.args([std::ffi::OsStr::new(INTERNAL_ARG), root.as_os_str()])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A caller may supply non-CLOEXEC pipes (including shell output capture).
    // In the child only, mark inherited descriptors to close on successful exec.
    let fd_dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let descriptors: Vec<i32> = fs::read_dir(fd_dir)?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter(|fd| *fd > 2)
        .collect();
    // Keep an interactive shell's signals separate from the watcher.
    unsafe {
        cmd.pre_exec(move || {
            unsafe extern "C" {
                fn setsid() -> i32;
                fn fcntl(fd: i32, cmd: i32, ...) -> i32;
            }
            for &fd in &descriptors {
                fcntl(fd, 2, 1); // F_SETFD, FD_CLOEXEC
            }
            if setsid() < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = cmd.spawn()?;
    let mut ready = [0];
    child.stdout.take().unwrap().read_exact(&mut ready)?;
    if ready == [0] {
        // Another starter holds the lock. Its listener may still be starting.
        child.wait()?;
        for _ in 0..20 {
            if let Ok(conn) = UnixStream::connect(&endpoint.socket) {
                return Ok(conn);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    UnixStream::connect(&endpoint.socket)
}

pub(crate) fn query(
    cwd: &Path,
    args: &[String],
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<Option<ExitCode>> {
    if !cfg!(target_os = "macos") || args.len() > 8 {
        return Ok(None);
    }
    let exchange = || -> io::Result<_> {
        let key = context(&executable_identity(&env::current_exe()?)?)?;
        let root = repository::locate(cwd)?;
        let mut socket = connect(&root, &Endpoint::new(&root)?)?;
        {
            let mut send = BufWriter::new(&mut socket);
            wire::send_field(&mut send, &key)?;
            wire::send_field(&mut send, cwd.as_os_str().as_bytes())?;
            send.write_all(&[args.len() as u8])?;
            for arg in args {
                wire::send_field(&mut send, arg.as_bytes())?;
            }
            send.flush()?;
        }
        let mut header = [0; 9];
        socket.read_exact(&mut header)?;
        Ok((
            socket,
            header[0],
            u64::from_le_bytes(header[1..].try_into().unwrap()),
        ))
    };
    let first = exchange();
    let answer = match first {
        Ok((_, REELECT, 0)) => exchange(),
        other => other,
    };
    let Ok((socket, code @ 0..=2, len)) = answer else {
        return Ok(None);
    };
    let mut reader = socket.take(len);
    let count = if code == 0 {
        let count = io::copy(&mut reader, out)?;
        out.flush()?;
        count
    } else {
        let count = io::copy(&mut reader, err)?;
        err.flush()?;
        count
    };
    if count != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete cx watcher response",
        ));
    }
    Ok(Some(ExitCode::from(code)))
}

enum Event {
    Query(UnixStream, wire::Query),
    Dirty,
}
#[derive(Clone)]
struct Wake {
    changed: Arc<AtomicBool>,
    tx: SyncSender<Event>,
}
impl Wake {
    #[cfg(target_os = "macos")]
    fn dirty(&self) {
        if !self.changed.swap(true, Ordering::AcqRel) {
            let _ = self.tx.try_send(Event::Dirty);
        }
    }
}
struct UnlinkSocket(PathBuf);
impl Drop for UnlinkSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn process(
    root: &Path,
    clients: Vec<(UnixStream, wire::Query)>,
    key: &[u8],
    current: &mut Option<Pin>,
) -> bool {
    let mut pin = None;
    let mut valid = false;
    for (mut socket, query) in clients {
        let _ = socket.set_write_timeout(Some(Duration::from_secs(5)));
        if query.context != key
            || !query.cwd.starts_with(root)
            || repository::locate(&query.cwd).as_deref().ok() != Some(root)
        {
            let _ = wire::reply(&mut socket, FALLBACK, &[]);
            continue;
        }
        let request = match Request::parse(&query.args) {
            Ok(req) => req,
            Err(e) => {
                let _ = wire::reply(
                    &mut socket,
                    2,
                    format!("{e}. Run 'cx help' for usage.\n").as_bytes(),
                );
                continue;
            }
        };
        valid = true;
        // All these clients were admitted before this observation. Any later
        // admission remains queued for a subsequent observation.
        let pin = pin.get_or_insert_with(|| Pin::refresh_with_prior(root, current.take()));
        let result = match pin {
            Ok(pin) => {
                let answer = evaluate(pin, request, &query.args[0], &query.cwd);
                pin.reused = true;
                answer
            }
            Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
        };
        match result {
            Ok(mut text) => {
                text.push('\n');
                let _ = wire::reply(&mut socket, 0, text.as_bytes());
            }
            Err(e) => {
                let _ = wire::reply(&mut socket, 1, format!("cx: {e}\n").as_bytes());
            }
        }
    }
    if let Some(observation) = pin {
        *current = observation.ok();
    }
    valid
}

pub(crate) fn serve(root: &Path, mut ready: impl Write) -> io::Result<()> {
    if repository::locate(root)? != root {
        return Err(io::Error::other("invalid watcher root"));
    }
    let endpoint = Endpoint::new(root)?;
    let lock: File = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&endpoint.lock)?;
    if lock.try_lock().is_err() {
        ready.write_all(&[0])?;
        return ready.flush();
    }
    let _ = fs::remove_file(&endpoint.socket);
    let listener = UnixListener::bind(&endpoint.socket)?;
    let _socket = UnlinkSocket(endpoint.socket);
    let (tx, rx) = mpsc::sync_channel(64);
    let wake = Wake {
        changed: Arc::new(AtomicBool::new(false)),
        tx: tx.clone(),
    };
    #[cfg(target_os = "macos")]
    if !native::start(root, wake.clone()) {
        return Err(io::Error::other("FSEvents unavailable"));
    }
    let executable_path = env::current_exe()?;
    let executable = executable_identity(&executable_path)?;
    let key = context(&executable)?;
    std::thread::Builder::new()
        .name("cx-admission".into())
        .spawn(move || {
            for socket in listener.incoming() {
                let Ok(mut socket) = socket else { break };
                let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                if let Ok(query) = wire::read_query(&mut BufReader::new(&mut socket)) {
                    if tx.send(Event::Query(socket, query)).is_err() {
                        break;
                    }
                }
            }
        })?;
    ready.write_all(&[1])?;
    ready.flush()?;
    let mut current = None;
    let mut deadline = IdleDeadline::from_request(Instant::now());
    loop {
        let event = if let Some(wait) = deadline.remaining(Instant::now()) {
            rx.recv_timeout(wait).ok()
        } else {
            // Clients may already be queued if the previous build spanned
            // the deadline. Give those requests a chance to renew the lease.
            rx.try_recv().ok()
        };
        let Some(event) = event else { break };
        let mut clients = Vec::new();
        for event in std::iter::once(event).chain(rx.try_iter().take(63)) {
            if let Event::Query(socket, query) = event {
                clients.push((socket, query));
            }
        }
        let dirty = wake.changed.swap(false, Ordering::AcqRel);
        // A package upgrade can replace the running binary, or a client may
        // reach this owner through a different installed version. Relinquish
        // ownership before allowing the successor to start a publication.
        let replaced = executable_identity(&executable_path).ok() != Some(executable);
        let foreign_build = |query: &wire::Query| {
            query
                .context
                .strip_prefix(PROTOCOL)
                .and_then(|tail| tail.get(..executable.len()))
                .is_some_and(|identity| identity != executable)
        };
        if replaced || clients.iter().any(|(_, query)| foreign_build(query)) {
            drop(current);
            drop(_socket);
            drop(lock);
            for (mut socket, query) in clients {
                let code = if foreign_build(&query) {
                    REELECT
                } else {
                    FALLBACK
                };
                let _ = wire::reply(&mut socket, code, &[]);
            }
            return Ok(());
        }
        if !root.join(".git").exists() {
            break;
        }
        if !clients.is_empty() {
            let admitted = Instant::now();
            if process(root, clients, &key, &mut current) {
                deadline = IdleDeadline::from_request(admitted);
            }
        } else if deadline.remaining(Instant::now()).is_none() {
            break;
        } else if dirty && current.is_some() && endpoint.cache.join("current").is_file() {
            // A user who clears the cache leaves it empty until the next query.
            // Foreground reconciliation retries/report errors. Background
            // failure leaves the last durable generation available.
            current = Pin::refresh_with_prior(root, current.take()).ok();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_admitted_commands_reset_five_minutes() {
        let start = Instant::now();
        let mut deadline = IdleDeadline::from_request(start);
        let at = |s| start + Duration::from_secs(s);
        assert_eq!(deadline.remaining(at(299)), Some(Duration::from_secs(1)));
        deadline = IdleDeadline::from_request(at(299));
        assert_eq!(deadline.remaining(at(300)), Some(Duration::from_secs(299)));
        assert_eq!(deadline.remaining(at(598)), Some(Duration::from_secs(1)));
        assert_eq!(deadline.remaining(at(599)), None);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn repeated_callbacks_bound_storage_to_one_pending_reconcile() {
        let (tx, rx) = mpsc::sync_channel(1);
        let wake = Wake {
            changed: Arc::new(AtomicBool::new(false)),
            tx,
        };
        for _ in 0..1024 {
            wake.dirty();
        }
        assert_eq!(rx.try_iter().count(), 1);
        assert!(wake.changed.swap(false, Ordering::AcqRel));
        wake.dirty();
        assert!(matches!(rx.try_recv(), Ok(Event::Dirty)));
    }
}
