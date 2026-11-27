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

struct Queue {
    jobs: Mutex<Jobs>,
    wake: Condvar,
}

impl Queue {
    fn next(&self) -> Option<Directory> {
        let state = self.jobs.lock().unwrap();
        let mut state = self
            .wake
            .wait_while(state, |state| {
                !state.stopped && state.ready.is_empty() && state.active != 0
            })
            .unwrap();
        if state.stopped {
            return None;
        }
        let dir = state.ready.pop()?;
        state.active += 1;
        Some(dir)
    }

    fn finish(&self, result: io::Result<()>, children: &mut Vec<Directory>) -> io::Result<()> {
        let mut state = self.jobs.lock().unwrap();
        state.active -= 1;
        if let Err(error) = result {
            state.stopped = true;
            self.wake.notify_all();
            return Err(error);
        }
        let added = !children.is_empty();
        state.ready.append(children);
        if added || state.active == 0 {
            self.wake.notify_all();
        }
        Ok(())
    }
}

pub(super) fn collect(
    tree: &Tree,
    ready: Vec<Directory>,
    workers: usize,
) -> io::Result<Vec<ObservedPath>> {
    let queue = Queue {
        jobs: Mutex::new(Jobs {
            ready,
            active: 0,
            stopped: false,
        }),
        wake: Condvar::new(),
    };
    let run = || -> io::Result<Vec<ObservedPath>> {
        let mut found = Vec::new();
        let mut children = Vec::new();
        while let Some(dir) = queue.next() {
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
            queue.finish(result, &mut children)?;
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
