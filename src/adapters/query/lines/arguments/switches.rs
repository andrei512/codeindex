//! Each switch defines its public spelling and the effect applied in argument order.
use super::*;

struct Switch {
    on: &'static str,
    off: &'static str,
    short: char,
    off_short: char,
    set: fn(&mut Options<'_>, bool),
}

const fn switch(on: &'static str, short: char, set: fn(&mut Options<'_>, bool)) -> Switch {
    Switch {
        on,
        off: "",
        short,
        off_short: '\0',
        set,
    }
}
const fn toggle(
    on: &'static str,
    off: &'static str,
    (short, off_short): (char, char),
    set: fn(&mut Options<'_>, bool),
) -> Switch {
    Switch {
        on,
        off,
        short,
        off_short,
        set,
    }
}

const ALL: &[Switch] = &[
    switch("cx-pattern-file", '\0', |q, _| q.pattern_file = true),
    switch("cx-pattern-stdin", '\0', |q, _| {
        q.query.pattern_stdin = true
    }),
    switch("no-config", '\0', |_, _| {}),
    switch("cx-terminal-color", '\0', |q, _| {
        q.query.terminal_color = true
    }),
    switch("sort-files", '\0', |q, _| {
        q.query.scope.sort = scope::Sort::Path;
        q.query.scope.reverse = false;
    }),
    switch("no-column", '\0', |q, _| q.query.column = false),
    switch("vimgrep", '\0', |q, _| {
        q.query.vimgrep = true;
        q.query.column = true;
        q.query.numbered = Some(true);
        q.query.filename = Some(true);
        q.query.heading = Some(false);
    }),
    switch("no-pre", '\0', |q, _| q.query.transform.program = None),
    switch("binary", '\0', |q, _| {
        q.query.binary = true;
        q.query.text = false;
    }),
    switch("no-binary", '\0', |q, _| {
        q.query.binary = false;
        q.query.text = false;
    }),
    switch("no-encoding", '\0', |q, _| {
        q.query.encoding = encoding::Encoding::None
    }),
    switch("auto-hybrid-regex", '\0', |q, _| q.matcher.engine = patterns::Engine::Auto),
    switch("no-auto-hybrid-regex", '\0', |q, _| q.matcher.engine = patterns::Engine::Native),
    switch("no-pcre2", '\0', |q, _| q.matcher.engine = patterns::Engine::Native),
    switch("files-without-match", '\0', |q, _| {
        q.query.result = ResultMode::NonmatchingPaths
    }),
    switch("count-matches", '\0', |q, _| {
        q.query.result = ResultMode::CountMatches
    }),
    switch("passthru", '\0', |q, _| {
        q.context = ContextOptions::passthru()
    }),
    switch("passthrough", '\0', |q, _| {
        q.context = ContextOptions::passthru()
    }),
    switch("no-context-separator", '\0', |q, _| {
        q.query.context_separator = None
    }),
    switch("cx-terminal", '\0', |q, _| q.query.terminal = true),
    switch("json", '\0', |q, _| q.query.json = true),
    toggle("hidden", "no-hidden", ('\0', '\0'), |q, enabled| {
        q.query.hidden = enabled
    }),
    switch("files", '\0', |q, _| q.query.files = true),
    switch("crlf", '\0', |q, _| q.query.crlf = true),
    toggle("heading", "no-heading", ('\0', '\0'), |q, enabled| {
        q.query.heading = Some(enabled)
    }),
    switch("column", '\0', |q, _| {
        q.query.column = true;
        q.query.numbered = Some(true);
    }),
    switch("type-list", '\0', |q, _| q.list_types = true),
    switch("pretty", 'p', |q, _| {
        q.query.color = Some(true);
        q.query.heading = Some(true);
        q.query.numbered = Some(true);
    }),
    toggle("follow", "no-follow", ('L', '\0'), |q, enabled| {
        q.query.walk.follow = enabled
    }),
    switch("unrestricted", 'u', |q, _| {
        q.unrestricted += 1;
        match q.unrestricted {
            1 => q.query.walk.no_ignore = true,
            2 => q.query.hidden = true,
            _ => q.query.binary = true,
        }
    }),
    toggle("search-zip", "no-search-zip", ('z', '\0'), |q, enabled| {
        q.query.transform.compressed = enabled
    }),
    switch("pcre2", 'P', |q, _| q.matcher.engine = patterns::Engine::Pcre),
    toggle("byte-offset", "no-byte-offset", ('b', '\0'), |q, enabled| {
        q.query.byte_offset = enabled
    }),
    toggle("multiline", "no-multiline", ('U', '\0'), |q, enabled| q.query.set_multiline(enabled)),
    toggle("null", "no-null", ('0', '\0'), |q, enabled| {
        q.query.null_path = enabled
    }),
    switch("text", 'a', |q, _| q.query.text = true),
    switch("word-regexp", 'w', |q, _| q.query.match_scope = crate::domain::pattern::MatchScope::Word),
    switch("line-regexp", 'x', |q, _| q.query.match_scope = crate::domain::pattern::MatchScope::Line),
    switch("only-matching", 'o', |q, _| q.query.only = true),
    switch("fixed-strings", 'F', |q, _| q.matcher.fixed = true),
    switch("ignore-case", 'i', |q, _| q.matcher.case = Case::Insensitive),
    switch("case-sensitive", 's', |q, _| q.matcher.case = Case::Sensitive),
    switch("smart-case", 'S', |q, _| q.matcher.case = Case::Smart),
    toggle("line-number", "no-line-number", ('n', 'N'), |q, enabled| {
        q.query.numbered = Some(enabled)
    }),
    switch("with-filename", 'H', |q, _| q.query.filename = Some(true)),
    switch("no-filename", 'I', |q, _| q.query.filename = Some(false)),
    switch("count", 'c', |q, _| q.query.result = ResultMode::CountLines),
    switch("files-with-matches", 'l', |q, _| {
        q.query.result = ResultMode::MatchingPaths
    }),
    switch("invert-match", 'v', |q, _| q.query.invert = true),
    switch("quiet", 'q', |q, _| q.query.quiet = true),
    toggle(
        "multiline-dotall",
        "no-multiline-dotall",
        ('\0', '\0'),
        |q, enabled| q.query.dotall = enabled,
    ),
    toggle("null-data", "no-null-data", ('\0', '\0'), |q, enabled| {
        q.query.null_data = enabled
    }),
    toggle("mmap", "no-mmap", ('\0', '\0'), |q, enabled| {
        q.query.mapped = enabled
    }),
    toggle("messages", "no-messages", ('\0', '\0'), |q, enabled| {
        q.query.no_messages = !enabled
    }),
    toggle(
        "one-file-system",
        "no-one-file-system",
        ('\0', '\0'),
        |q, enabled| q.query.walk.one_device = enabled,
    ),
    toggle(
        "ignore-global",
        "no-ignore-global",
        ('\0', '\0'),
        |q, enabled| q.query.walk.no_global = !enabled,
    ),
    toggle("ignore", "no-ignore", ('\0', '\0'), |q, enabled| {
        q.query.walk.no_ignore = !enabled
    }),
    toggle(
        "ignore-parent",
        "no-ignore-parent",
        ('\0', '\0'),
        |q, enabled| q.query.walk.no_parent = !enabled,
    ),
    toggle("ignore-dot", "no-ignore-dot", ('\0', '\0'), |q, enabled| {
        q.query.walk.no_dot = !enabled
    }),
    toggle("ignore-vcs", "no-ignore-vcs", ('\0', '\0'), |q, enabled| {
        q.query.walk.no_vcs = !enabled
    }),
    toggle(
        "ignore-exclude",
        "no-ignore-exclude",
        ('\0', '\0'),
        |q, enabled| q.query.walk.no_exclude = !enabled,
    ),
    toggle(
        "ignore-files",
        "no-ignore-files",
        ('\0', '\0'),
        |q, enabled| q.query.walk.no_files = !enabled,
    ),
    toggle("require-git", "no-require-git", ('\0', '\0'), |q, enabled| {
        q.query.walk.no_require_git = !enabled
    }),
    toggle(
        "ignore-file-case-insensitive",
        "no-ignore-file-case-insensitive",
        ('\0', '\0'),
        |q, enabled| q.query.walk.fold = enabled,
    ),
    toggle(
        "max-columns-preview",
        "no-max-columns-preview",
        ('\0', '\0'),
        |q, enabled| q.query.preview = enabled,
    ),
    toggle("stats", "no-stats", ('\0', '\0'), |q, enabled| {
        q.query.stats = enabled
    }),
    toggle("unicode", "no-unicode", ('\0', '\0'), |q, enabled| {
        q.query.ascii = !enabled
    }),
    toggle(
        "pcre2-unicode",
        "no-pcre2-unicode",
        ('\0', '\0'),
        |q, enabled| q.query.ascii = !enabled,
    ),
    toggle(
        "include-zero",
        "no-include-zero",
        ('\0', '\0'),
        |q, enabled| q.query.include_zero = enabled,
    ),
    toggle("trim", "no-trim", ('\0', '\0'), |q, enabled| {
        q.query.trim = enabled
    }),
    toggle(
        "stop-on-nonmatch",
        "no-stop-on-nonmatch",
        ('\0', '\0'),
        |q, enabled| q.query.set_stop_on_nonmatch(enabled),
    ),
    toggle(
        "glob-case-insensitive",
        "no-glob-case-insensitive",
        ('\0', '\0'),
        |q, enabled| q.glob_case = enabled,
    ),
];
pub(crate) fn spellings() -> impl Iterator<Item = (&'static str, char)> {
    ALL.iter()
        .flat_map(|s| [(s.on, s.short), (s.off, s.off_short)])
        .filter(|(name, _)| !name.is_empty())
}
pub(super) fn apply(options: &mut Options<'_>, flag: &str) -> Result<(), String> {
    let name = flag
        .strip_prefix("--")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("unrecognized flag {flag}"))?;
    let switch = ALL
        .iter()
        .find(|s| s.on == name || s.off == name)
        .ok_or_else(|| format!("unrecognized flag {flag}"))?;
    (switch.set)(options, switch.on == name);
    Ok(())
}
