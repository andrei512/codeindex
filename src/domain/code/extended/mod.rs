//! Native language profiles share the existing source-span and per-file evidence contracts.
pub(in crate::domain::code) mod analyzer;
pub(super) mod objects;
pub(super) use objects::tokens_with_comments as object_comments;
pub(super) mod build_langs;
pub(super) use build_langs::{config_comments, docker_comments, make_comments};
pub(super) mod scripts;
pub(super) use scripts::{body_comments, mako_comments, tokens_with_comments as script_comments};

pub(super) use scripts::sql;

/// Yield characters outside backslash escape pairs, retaining byte positions.
fn unescaped_chars(text: &str) -> impl Iterator<Item = (usize, char)> {
    let mut escaped = false;
    text.char_indices().filter(move |&(_, ch)| {
        let was_escaped = escaped;
        escaped = !was_escaped && ch == '\\';
        !(was_escaped || escaped)
    })
}
