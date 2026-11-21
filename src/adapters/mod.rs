//! Outer implementations of application ports. Register concrete adapters at
//! the CLI composition root. Keep format rules out of the scheduler.
pub mod cli;
mod query;
pub mod repository;
pub mod snapshot;
mod watcher;
