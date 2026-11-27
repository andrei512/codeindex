use super::span;
use crate::domain::code::Analysis;
mod batch;
mod docker;
mod make;

pub(in crate::domain::code) use batch::batch;
pub(in crate::domain::code) use docker::{docker, docker_comments};
pub(in crate::domain::code) use make::{make, make_comments};

pub(in crate::domain::code) fn procfile(source: &str) -> Analysis {
    let mut analysis = Analysis::default();
    for line in source.lines() {
        if let Some((name, command)) = line.split_once(':')
            && !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            && !command.trim().is_empty()
        {
            analysis.declare_at(span(source, name), "process_type");
        }
    }
    analysis
}
