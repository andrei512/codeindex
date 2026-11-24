//! Independent directories share immutable inherited ignore rules.
use super::{Directory, ObservedPath, Tree, Walk};
use std::{
    io,
    sync::{Condvar, Mutex},
};
struct Jobs {
    ready: Vec<Directory>,
    active: usize,
    stopped: bool,
}

pub(super) fn collect(
    tree: &Tree,
    ready: Vec<Directory>,
    workers: usize,
) -> io::Result<Vec<ObservedPath>> {
    let jobs = Mutex::new(Jobs {
        ready,
        active: 0,
        stopped: false,
    });
    let wake = Condvar::new();
    let run = || -> io::Result<Vec<ObservedPath>> {
        let mut found = Vec::new();
        let mut children = Vec::new();
        loop {
            let mut state = jobs.lock().unwrap();
            while state.ready.is_empty() && state.active != 0 && !state.stopped {
                state = wake.wait(state).unwrap();
            }
            if state.stopped {
                break;
            }
            let Some(dir) = state.ready.pop() else { break };
            state.active += 1;
            drop(state);
            let mut walk = Walk {
                tree,
                rules: dir.rules,
                names: dir.names,
                found,
            };
            // A panicking directory visitor must wake sleeping peers before
            // the scoped threads are joined.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                walk.visit(&dir.rel, dir.ignored, Some(&mut children))
            }))
            .unwrap_or_else(|_| Err(io::Error::other("directory worker panicked")));
            found = walk.found;
            let mut state = jobs.lock().unwrap();
            state.active -= 1;
            if let Err(e) = result {
                state.stopped = true;
                wake.notify_all();
                return Err(e);
            }
            let added = children.len();
            state.ready.append(&mut children);
            if added != 0 || state.active == 0 {
                wake.notify_all();
            }
        }
        Ok(found)
    };
    std::thread::scope(|scope| {
        let mut peers = Vec::new();
        for _ in 1..workers {
            peers.push(scope.spawn(run));
        }
        let mut found = run()?;
        for peer in peers {
            found.extend(peer.join().unwrap()?);
        }
        Ok(found)
    })
}
