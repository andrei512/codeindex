//! One speculative writer per worktree. Requests on a live owner reconcile
//! through the same Pin::refresh entry point as a standalone invocation.
#[cfg(target_os = "macos")]
mod native;
mod wire;
use super::{
    query::{Invocation, Outcome, Request},
    repository,
    snapshot::Pin,
};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
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
use wire::Response;

pub(crate) const INTERNAL_ARG: &str = "--internal-watch";
const IDLE: Duration = Duration::from_secs(5 * 60);
const PROTOCOL: &[u8] = b"CXW2";

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

fn runtime_directory() -> io::Result<PathBuf> {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    let uid = unsafe { geteuid() };
    // Darwin's sockaddr_un path is only 104 bytes. A user-private short
    // runtime directory also works for arbitrarily long HOME/worktree paths.
    let runtime = PathBuf::from(format!("/tmp/cx-{uid}"));
    private_runtime(&runtime, uid)?;
    Ok(runtime)
}

fn private_runtime(path: &Path, uid: u32) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => create_runtime(path)?,
        result => result?,
    };
    let owned_directory = meta.is_dir() && meta.uid() == uid;
    if !owned_directory || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other("unsafe cx watcher directory"));
    }
    Ok(())
}

// Another client may create the directory between inspection and mkdir.
fn create_runtime(path: &Path) -> io::Result<fs::Metadata> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => fs::symlink_metadata(path),
    }
}
impl Endpoint {
    fn new(root: &Path) -> io::Result<Self> {
        let runtime = runtime_directory()?;
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

fn start_owner(root: &Path) -> io::Result<(std::process::Child, [u8; 1])> {
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
    Ok((child, ready))
}

fn connect(root: &Path, endpoint: &Endpoint) -> io::Result<UnixStream> {
    if let Ok(conn) = UnixStream::connect(&endpoint.socket) {
        return Ok(conn);
    }
    let (mut child, ready) = start_owner(root)?;
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

pub(crate) struct CallerQuery<'a> {
    pub root: &'a Path,
    pub cwd: &'a Path,
    pub args: &'a [String],
}

fn request(query: CallerQuery<'_>) -> io::Result<(UnixStream, Response, u64)> {
    let exchange = || -> io::Result<_> {
        let key = context(&executable_identity(&env::current_exe()?)?)?;
        let mut socket = connect(query.root, &Endpoint::new(query.root)?)?;
        wire::write_query(&mut socket, &key, query.cwd, query.args)?;
        let (code, len) = wire::response_header(&mut socket)?;
        Ok((socket, code, len))
    };
    match exchange()? {
        (_, Response::Reelect, 0) => exchange(),
        answer => Ok(answer),
    }
}

pub(crate) fn query(
    query: CallerQuery<'_>,
    (out, err): (&mut impl Write, &mut impl Write),
) -> io::Result<Option<ExitCode>> {
    if !cfg!(target_os = "macos") || query.args.len() > 8 {
        return Ok(None);
    }
    let Ok((mut socket, code, len)) = request(query) else {
        return Ok(None);
    };
    match code {
        Response::InvalidArgs => {
            write!(err, "{}: ", super::cli::name())?;
            wire::copy_frame(&mut socket, len, err)?;
            Ok(Some(ExitCode::from(2)))
        }
        Response::Diagnostic(exit) => {
            wire::copy_frame(&mut socket, len, err)?;
            Ok(Some(ExitCode::from(exit)))
        }
        Response::Output(exit) => {
            wire::read_output(&mut socket, len, out, err)?;
            Ok(Some(ExitCode::from(exit)))
        }
        Response::Fallback | Response::Reelect => Ok(None),
    }
}

struct Client {
    socket: UnixStream,
    query: wire::Query,
}

enum Event {
    Query(Client),
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
/// Unlink the listener before releasing the election lock. A successor may
/// bind the same pathname as soon as that lock is released.
struct Ownership {
    socket: PathBuf,
    _lock: File,
}
impl Ownership {
    fn acquire(endpoint: &Endpoint) -> io::Result<Option<(Self, UnixListener)>> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&endpoint.lock)?;
        if lock.try_lock().is_err() {
            return Ok(None);
        }
        let _ = fs::remove_file(&endpoint.socket);
        let listener = UnixListener::bind(&endpoint.socket)?;
        Ok(Some((
            Self {
                socket: endpoint.socket.clone(),
                _lock: lock,
            },
            listener,
        )))
    }
}
impl Drop for Ownership {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}

fn reply_to_query(socket: &mut UnixStream, pin: &io::Result<Pin>, request: Invocation, cwd: &Path) {
    let mut outcome = Outcome::default();
    let is_lines = matches!(request.request, Request::Lines(_));
    let result = request.after_refresh(pin.as_ref(), cwd, &mut outcome);
    match result {
        Ok(mut text) => {
            if !text.is_empty() && !is_lines {
                text.push(b'\n');
            }
            let _ = wire::write_output(socket, outcome.code, &text, outcome.diagnostics.as_bytes());
        }
        Err(e) => {
            let _ = wire::reply(
                socket,
                Response::Diagnostic(2),
                format!("cx: {e}\n").as_bytes(),
            );
        }
    }
}

impl Session<'_> {
    fn admit<'a>(&self, socket: &mut UnixStream, query: &'a wire::Query) -> Option<Invocation<'a>> {
        let _ = socket.set_write_timeout(Some(Duration::from_secs(5)));
        if query.context != self.key
            || !query.cwd.starts_with(self.root)
            || repository::locate(&query.cwd).as_deref().ok() != Some(self.root)
        {
            let _ = wire::reply(socket, Response::Fallback, &[]);
            return None;
        }
        match Invocation::parse(&query.args) {
            Ok(request) => Some(request),
            Err(e) => {
                let _ = wire::reply(socket, Response::InvalidArgs, format!("{e}\n").as_bytes());
                None
            }
        }
    }
    fn process(&mut self, clients: &mut Vec<Client>) -> bool {
        let mut pin = None;
        for Client { mut socket, query } in clients.drain(..) {
            let Some(request) = self.admit(&mut socket, &query) else {
                continue;
            };
            // The first accepted request may publish. The rest observe that generation.
            let held = pin.get_or_insert_with(|| {
                Pin::refresh_root(self.root.to_owned(), self.current.take())
            });
            reply_to_query(&mut socket, held, request, &query.cwd);
            if let Ok(held) = held {
                held.reused = true;
            }
        }
        let Some(pin) = pin else {
            return false;
        };
        self.current = pin.ok();
        true
    }
}

pub(crate) fn serve(root: &Path, mut ready: impl Write) -> io::Result<()> {
    if repository::locate(root)? != root {
        return Err(io::Error::other("invalid watcher root"));
    }
    let endpoint = Endpoint::new(root)?;
    let Some((owner, listener)) = Ownership::acquire(&endpoint)? else {
        ready.write_all(&[0])?;
        return ready.flush();
    };
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
    admit_connections(listener, tx)?;
    ready.write_all(&[1])?;
    ready.flush()?;
    let mut state = Session {
        root,
        key: &key,
        current: None,
        deadline: IdleDeadline::from_request(Instant::now()),
        marker: root.join(".git"),
        published: endpoint.cache.join("current"),
    };
    if let Some(replies) = state.run(&rx, &wake, &executable_path, &executable) {
        // Release the pin and listener ownership before a client can elect a successor.
        drop(state);
        drop(owner);
        for (mut socket, response) in replies {
            let _ = wire::reply(&mut socket, response, &[]);
        }
    }
    Ok(())
}

/// On handoff, classify each client once and release its captured query.
fn retirement(
    clients: &mut Vec<Client>,
    executable: &[u8],
    replaced: bool,
) -> Option<Vec<(UnixStream, Response)>> {
    let first_foreign = if replaced {
        None
    } else {
        Some(
            clients
                .iter()
                .position(|client| foreign_build(&client.query, executable))?,
        )
    };
    let replies = clients
        .drain(..)
        .enumerate()
        .map(|(index, Client { socket, query })| {
            // The search already classified the prefix and its first foreign client.
            let foreign = match first_foreign {
                Some(first) if index <= first => index == first,
                _ => foreign_build(&query, executable),
            };
            (
                socket,
                if foreign {
                    Response::Reelect
                } else {
                    Response::Fallback
                },
            )
        })
        .collect();
    Some(replies)
}

fn foreign_build(q: &wire::Query, executable: &[u8]) -> bool {
    q.context
        .strip_prefix(PROTOCOL)
        .and_then(|tail| tail.get(..executable.len()))
        .is_some_and(|identity| identity != executable)
}

impl IdleDeadline {
    fn receive(&self, rx: &mpsc::Receiver<Event>, clients: &mut Vec<Client>) -> bool {
        // Already admitted clients may renew the lease even when a build spans the deadline.
        let first = match self.remaining(Instant::now()) {
            Some(wait) => rx.recv_timeout(wait).ok(),
            None => rx.try_recv().ok(),
        };
        let Some(event) = first else {
            return false;
        };
        clients.extend(
            std::iter::once(event)
                .chain(rx.try_iter().take(63))
                .filter_map(|e| match e {
                    Event::Query(client) => Some(client),
                    Event::Dirty => None,
                }),
        );
        true
    }
}

struct Session<'a> {
    root: &'a Path,
    key: &'a [u8],
    current: Option<Pin>,
    deadline: IdleDeadline,
    marker: PathBuf,
    published: PathBuf,
}

impl Session<'_> {
    fn run(
        &mut self,
        rx: &mpsc::Receiver<Event>,
        wake: &Wake,
        executable_path: &Path,
        executable: &[u8; 32],
    ) -> Option<Vec<(UnixStream, Response)>> {
        let mut clients = Vec::new();
        loop {
            self.deadline.receive(rx, &mut clients).then_some(())?;
            let dirty = wake.changed.swap(false, Ordering::AcqRel);
            let replaced = executable_identity(executable_path).as_ref().ok() != Some(executable);
            if let Some(replies) = retirement(&mut clients, executable, replaced) {
                return Some(replies);
            }
            self.observe(&mut clients, dirty).then_some(())?;
        }
    }

    fn observe(&mut self, clients: &mut Vec<Client>, dirty: bool) -> bool {
        if !self.marker.exists() {
            return false;
        }
        let admitted = Instant::now();
        if self.process(clients) {
            self.deadline = IdleDeadline::from_request(admitted);
            return true;
        }
        if self.deadline.remaining(admitted).is_none() {
            return false;
        }
        if let Some(prior) = self.current.take_if(|_| dirty && self.published.is_file()) {
            // Keep a cleared cache empty. The next foreground query retries failed refreshes.
            self.current = Pin::refresh_with_prior(self.root, Some(prior)).ok();
        }
        true
    }
}

fn admit_connections(listener: UnixListener, tx: SyncSender<Event>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("cx-admission".into())
        .spawn(move || receive_connections(listener.incoming(), tx))?;
    Ok(())
}

enum Admission {
    Query(Client),
    Continue,
    Retired,
}

impl Admission {
    fn dispatch(self, tx: &SyncSender<Event>) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow;
        let continuing = match self {
            Self::Query(client) => tx.send(Event::Query(client)).is_ok(),
            Self::Continue => true,
            Self::Retired => false,
        };
        if continuing { ControlFlow::Continue(()) } else { ControlFlow::Break(()) }
    }

    fn receive(accepted: io::Result<UnixStream>) -> Self {
        let mut socket = match accepted {
            Ok(socket) => socket,
            Err(err) => {
                return match err.kind() {
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted => Self::Continue,
                    _ => Self::Retired,
                };
            }
        };
        let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
        match wire::read_query(&mut BufReader::new(&mut socket)) {
            Ok(query) => Self::Query(Client { socket, query }),
            Err(_) => Self::Continue,
        }
    }
}

fn receive_connections(
    incoming: impl Iterator<Item = io::Result<UnixStream>>,
    tx: SyncSender<Event>,
) {
    let _ = incoming.map(Admission::receive).try_for_each(|admission| admission.dispatch(&tx));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_runtime_admits_only_the_current_owners_private_directory() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = std::env::temp_dir().join(format!("cx-runtime-test-{}", std::process::id()));
        fs::create_dir(&temp).unwrap();
        let uid = fs::metadata(&temp).unwrap().uid();
        let path = temp.join("runtime");
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| private_runtime(&path, uid).unwrap());
            }
        });
        let meta = fs::symlink_metadata(&path).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o700);
        assert!(private_runtime(&path, uid.wrapping_add(1)).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_runtime(&path, uid).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&path, temp.join("link")).unwrap();
        assert!(private_runtime(&temp.join("link"), uid).is_err());
        fs::write(temp.join("file"), b"").unwrap();
        assert!(private_runtime(&temp.join("file"), uid).is_err());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn admission_continues_after_transient_accept_errors_but_stops_on_terminal_errors() {
        fn framed(arg: &str) -> io::Result<UnixStream> {
            let (mut client, server) = UnixStream::pair()?;
            wire::write_query(
                &mut client,
                b"context",
                Path::new("/tmp"),
                &[arg.to_owned()],
            )?;
            Ok(server)
        }
        let (closed, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let one = std::iter::once(framed("disconnected receiver"));
        receive_connections(
            one.chain(std::iter::from_fn(|| panic!("read past closed queue"))),
            closed,
        );

        let (malformed, truncated) = UnixStream::pair().unwrap();
        drop(malformed);
        let (tx, rx) = mpsc::sync_channel(4);
        let incoming = [
            Err(io::ErrorKind::Interrupted.into()),
            Ok(truncated),
            framed("first"),
            Err(io::ErrorKind::ConnectionAborted.into()),
            framed("second"),
            Err(io::ErrorKind::PermissionDenied.into()),
            framed("after terminal error"),
        ];
        receive_connections(incoming.into_iter(), tx);
        let admitted: Vec<_> = rx
            .into_iter()
            .map(|event| match event {
                Event::Query(client) => client.query.args,
                Event::Dirty => panic!("unexpected dirty event"),
            })
            .collect();
        assert_eq!(
            admitted,
            [vec!["first".to_owned()], vec!["second".to_owned()]]
        );
    }

    #[test]
    fn retirement_replies_preserve_batch_order_and_context_ownership() {
        let current = [5u8; 32];
        let key = |id: &[u8]| [&PROTOCOL[..], id, b"env"].concat();
        let inputs = [
            key(&current),
            key(&[9u8; 32]),
            b"unknown".to_vec(),
            [&PROTOCOL[..], &[1u8; 4]].concat(),
            key(&[2u8; 32]),
            key(&current),
        ];
        for replaced in [false, true] {
            let mut peers = Vec::new();
            let mut clients = inputs
                .iter()
                .map(|context| {
                    let (peer, socket) = UnixStream::pair().unwrap();
                    peers.push(peer);
                    Client {
                        socket,
                        query: wire::Query {
                            context: context.clone(),
                            cwd: PathBuf::new(),
                            args: Vec::new(),
                        },
                    }
                })
                .collect();
            let replies = retirement(&mut clients, &current, replaced).unwrap();
            assert!(clients.is_empty());
            for (mut socket, code) in replies {
                wire::reply(&mut socket, code, &[]).unwrap();
            }
            let actual = peers
                .iter_mut()
                .map(|p| matches!(wire::response_header(p).unwrap(), (Response::Reelect, 0)))
                .collect::<Vec<_>>();
            assert_eq!(actual, [false, true, false, false, true, false]);
        }
        let (_, socket) = UnixStream::pair().unwrap();
        let mut local = vec![Client {
            socket,
            query: wire::Query {
                context: key(&current),
                cwd: PathBuf::new(),
                args: Vec::new(),
            },
        }];
        assert!(retirement(&mut local, &current, false).is_none());
        assert_eq!(local.len(), 1);
    }

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
    #[test]
    fn rejected_query_does_not_keep_an_expired_session_alive() {
        let root = env::temp_dir();
        let mut session = Session {
            root: &root,
            key: b"owner",
            current: None,
            deadline: IdleDeadline(Instant::now()),
            marker: root.clone(),
            published: root.join("current"),
        };
        let (server, mut client) = UnixStream::pair().unwrap();
        let rejected = wire::Query {
            context: b"other".to_vec(),
            cwd: root.clone(),
            args: Vec::new(),
        };
        let mut clients = vec![Client {
            socket: server,
            query: rejected,
        }];
        assert!(!session.observe(&mut clients, false));
        assert!(matches!(
            wire::response_header(&mut client).unwrap(),
            (Response::Fallback, 0)
        ));
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
