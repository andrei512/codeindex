//! Owned work may outlive the builder's borrowed inputs. Join it on every exit
//! so a retry never overlaps a previous attempt's file writes or reclamation.
use std::{
    io,
    thread::{self, JoinHandle},
};

pub(crate) struct Worker<T>(Option<JoinHandle<io::Result<T>>>);

impl<T: Send + 'static> Worker<T> {
    pub(crate) fn start(job: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<Self> {
        thread::Builder::new().spawn(job).map(|h| Self(Some(h)))
    }
}
impl<T> Worker<T> {
    pub(crate) fn finish(mut self) -> io::Result<T> {
        self.0
            .take()
            .unwrap()
            .join()
            .map_err(|_| io::Error::other("index worker panicked"))?
    }
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            let _ = job.join();
        }
    }
}
