//! A yielded job keeps its original charge. When no new job fits, a worker
//! resumes an admitted job. Releasing provisional output never requires credit.
use super::{Jobs, Queue, run_workers};
use std::{
    collections::VecDeque,
    io,
    sync::{Condvar, Mutex},
};

struct State<T> {
    fresh: Queue<T>,
    ready: VecDeque<(T, usize)>,
    active: usize,
    cancelled: bool,
}

impl<T> State<T> {
    fn take(&mut self) -> Option<(T, usize)> {
        let work = self.fresh.take().or_else(|| self.ready.pop_front())?;
        self.active += 1;
        Some(work)
    }

    fn wait_for_job(state: &Mutex<Self>, event: &Condvar) -> Option<(T, usize)> {
        let mut queue = state.lock().unwrap();
        while !queue.cancelled {
            let work = queue.take();
            if work.is_some() || queue.active == 0 {
                return work;
            }
            queue = event.wait(queue).unwrap();
        }
        None
    }

    fn finish(&mut self, charge: usize, result: io::Result<Option<T>>) -> io::Result<()> {
        self.active -= 1;
        match result {
            Ok(Some(next)) => self.ready.push_back((next, charge)),
            Ok(None) => self.fresh.available += charge,
            Err(error) => {
                self.cancelled = true;
                return Err(error);
            }
        }
        Ok(())
    }
}

impl<T: Send> Jobs<T> {
    pub(in crate::application::index::build) fn run_steps(
        self,
        width: usize,
        step: impl Fn(T) -> io::Result<Option<T>> + Sync,
    ) -> io::Result<()> {
        if self.count == 0 {
            return Ok(());
        }
        let workers = width.min(self.count);
        if workers == 1 || self.fits_concurrently(workers) {
            // Every lane already has admission; finish its borrowed work locally.
            // The shared ready queue is needed only when jobs contend for credit.
            return self.run(width, |mut job| {
                while let Some(next) = step(job)? {
                    job = next;
                }
                Ok(())
            });
        }
        let state = Mutex::new(State {
            fresh: Queue {
                by_size: self.by_size,
                available: self.allowance,
            },
            ready: VecDeque::new(),
            active: 0,
            cancelled: false,
        });
        let event = Condvar::new();
        run_workers(0..workers, "analysis worker failed", |_| {
            while let Some((job, charge)) = State::wait_for_job(&state, &event) {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| step(job)))
                    .unwrap_or_else(|_| Err(io::Error::other("analysis worker failed")));
                let finished = state.lock().unwrap().finish(charge, result);
                event.notify_all();
                finished?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suspended_jobs_finish_when_one_consumes_the_entire_budget() {
        let done = std::sync::atomic::AtomicUsize::new(0);
        let mut jobs = Jobs::new(5);
        for cost in [1, 2, 3, 5, 50] {
            jobs.push(cost, 4);
        }
        jobs.run_steps(3, |left| {
            if left == 0 {
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(None)
            } else {
                Ok(Some(left - 1))
            }
        })
        .unwrap();
        assert_eq!(done.into_inner(), 5);
    }
}
