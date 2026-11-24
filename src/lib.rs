//! Local code and Markdown indexing for the `cx` CLI.
//!
//! Install with `cargo install --locked codeindex`. Inside a Git working tree,
//! run `cx -k 'retry policy'` for ranked passages or `cx --symbol RetryPolicy`
//! for declarations. Run `cx help` for query formats and semantic coverage.
pub mod adapters;
pub mod application;
pub mod domain;
