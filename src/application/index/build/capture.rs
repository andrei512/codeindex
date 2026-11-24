//! Schedule file captures within their shared transient byte allowance.
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
    io,
    sync::{Condvar, Mutex},
    thread,
};

pub(super) struct Jobs<T> {
    by_size: BTreeMap<usize, Vec<T>>,
    allowance: usize,
    count: usize,
}

impl<T: Send> Jobs<T> {
    pub(super) fn new(allowance: usize) -> Self {
        debug_assert!(allowance > 0);
        Self {
            by_size: BTreeMap::new(),
            allowance,
            count: 0,
        }
    }

    pub(super) fn push(&mut self, bytes: u64, job: T) {
        // An oversized input may run alone.
        let cost = usize::try_from(bytes)
            .unwrap_or(usize::MAX)
            .clamp(1, self.allowance);
        self.by_size.entry(cost).or_default().push(job);
        self.count += 1;
    }

    fn fits_concurrently(&self, workers: usize) -> bool {
        // Repeated costs count separately. Checking the largest possible active
        // set proves that even the worst overlap needs no credit coordination.
        self.by_size
            .iter()
            .rev()
            .flat_map(|(&cost, group)| std::iter::repeat_n(cost, group.len()))
            .take(workers)
            .try_fold(self.allowance, usize::checked_sub)
            .is_some()
    }

    pub(super) fn run(
        self,
        width: usize,
        process: impl Fn(T) -> io::Result<()> + Sync,
    ) -> io::Result<()> {
        if self.count == 0 {
            return Ok(());
        }
        let workers = width.min(self.count);
        if self.fits_concurrently(workers) {
            run_workers(self.lanes(workers), |lane| {
                lane.into_iter().try_for_each(&process)
            })
        } else {
            let state = Mutex::new(Queue {
                by_size: self.by_size,
                available: self.allowance,
            });
            let ready = Condvar::new();
            run_workers(0..workers, |_| {
                while let Some((job, _credit)) = reserve(&state, &ready) {
                    process(job)?;
                }
                Ok(())
            })
        }
    }

    fn lanes(self, workers: usize) -> Vec<Vec<T>> {
        let mut lanes: Vec<Vec<T>> = (0..workers).map(|_| Vec::new()).collect();
        let mut loads: BinaryHeap<_> = (0..workers).map(|id| Reverse((0u128, id))).collect();
        // Longest inputs first. A small per-file floor also balances open
        // and parser setup costs in trees dominated by tiny files.
        for (cost, group) in self.by_size.into_iter().rev() {
            for job in group {
                let mut lightest = loads.peek_mut().unwrap();
                let Reverse((weight, id)) = &mut *lightest;
                lanes[*id].push(job);
                *weight += cost.max(1024) as u128;
            }
        }
        lanes
    }
}

fn run_workers<W: Send>(
    work: impl IntoIterator<Item = W>,
    run: impl Fn(W) -> io::Result<()> + Sync,
) -> io::Result<()> {
    thread::scope(|scope| {
        let run = &run;
        let handles: Vec<_> = work
            .into_iter()
            .map(|item| scope.spawn(move || run(item)))
            .collect();
        // Join every worker. A second panic left for scope teardown would
        // otherwise hide the first worker's error.
        let mut result = Ok(());
        for handle in handles {
            let finished = handle
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("capture worker failed")));
            result = result.and(finished);
        }
        result
    })
}

fn reserve<'a, T>(
    state: &'a Mutex<Queue<T>>,
    ready: &'a Condvar,
) -> Option<(T, Reservation<'a, T>)> {
    let mut queue = state.lock().unwrap();
    loop {
        if queue.by_size.is_empty() {
            return None;
        }
        if let Some((job, credit)) = queue.take() {
            return Some((
                job,
                Reservation {
                    state,
                    ready,
                    credit,
                },
            ));
        }
        queue = ready.wait(queue).unwrap();
    }
}

struct Queue<T> {
    by_size: BTreeMap<usize, Vec<T>>,
    available: usize,
}

impl<T> Queue<T> {
    fn take(&mut self) -> Option<(T, usize)> {
        let cost = *self.by_size.range(..=self.available).next_back()?.0;
        let group = self.by_size.get_mut(&cost).unwrap();
        let job = group.pop().unwrap();
        if group.is_empty() {
            self.by_size.remove(&cost);
        }
        self.available -= cost;
        Some((job, cost))
    }
}

// Return credit on error and on unwind so the other workers can drain the queue.
struct Reservation<'a, T> {
    state: &'a Mutex<Queue<T>>,
    ready: &'a Condvar,
    credit: usize,
}

impl<T> Drop for Reservation<'_, T> {
    fn drop(&mut self) {
        self.state.lock().unwrap().available += self.credit;
        self.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_large_jobs_require_credit_even_when_the_sum_would_overflow() {
        let mut jobs = Jobs::new(usize::MAX);
        jobs.push(u64::MAX, ());
        jobs.push(u64::MAX, ());
        assert!(jobs.fits_concurrently(1));
        assert!(!jobs.fits_concurrently(2));
    }

    #[test]
    fn capture_panic_releases_the_credit_needed_by_another_worker() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let mut jobs = Jobs::new(1);
        jobs.push(100, ());
        jobs.push(100, ());
        let error = jobs
            .run(2, |_| {
                if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    panic!("injected capture panic");
                }
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "capture worker failed");
        assert_eq!(calls.into_inner(), 2);
    }
}
