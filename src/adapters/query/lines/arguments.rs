//! Interpret line-search operands once, in invocation order. Only the finalized
//! query and its compiled matchers survive argument parsing.
mod switches;
mod values;
pub(crate) use values::ValueOption;
use super::{Lines, ResultMode, encoding, operands, patterns, scope, types};

pub(crate) use switches::spellings;

use patterns::Case;

#[derive(Clone, Copy)]
enum ContextSide {
    Before,
    After,
    Both,
}

/// A side-specific width overrides the common width regardless of argument order.
/// Passthrough discards all previously requested widths.
#[derive(Default)]
struct ContextOptions {
    common: usize,
    before: Option<usize>,
    after: Option<usize>,
    passthru: bool,
}

impl ContextOptions {
    fn passthru() -> Self {
        Self {
            passthru: true,
            ..Self::default()
        }
    }

    fn set(&mut self, side: ContextSide, width: usize) {
        self.passthru = false;
        match side {
            ContextSide::Before => self.before = Some(width),
            ContextSide::After => self.after = Some(width),
            ContextSide::Both => self.common = width,
        }
    }

    fn apply(self, query: &mut Lines<'_>) {
        query.before = self.before.unwrap_or(self.common);
        query.after = self.after.unwrap_or(self.common);
        query.passthru = self.passthru;
    }
}

pub(crate) enum CallerOutput {
    Types,
    Empty,
    Stdin,
}

#[derive(Default)]
pub(crate) struct Options<'a> {
    query: Lines<'a>,
    context: ContextOptions,
    matcher: patterns::MatcherOptions,
    patterns: Vec<&'a str>,
    edits: Vec<types::Edit<'a>>,
    selections: Vec<(bool, &'a str)>,
    list_types: bool,
    pattern_file: bool,
    replacement: Option<&'a str>,
    unrestricted: usize,
    glob_case: bool,
    glob_specs: Vec<(&'a str, bool)>,
}

impl<'a> Options<'a> {
    pub(crate) fn parse(input: &'a [String]) -> Result<Self, String> {
        let query = Lines {
            match_separator: b":".to_vec(),
            context_field: b"-".to_vec(),
            context_separator: Some(b"--".to_vec()),
            ..Lines::default()
        };
        let mut options = Options {
            query,
            ..Options::default()
        };
        let mut args = input.iter().map(String::as_str);
        while let Some(arg) = args.next() {
            if arg == "--" {
                options.positional(args);
                break;
            }
            options.apply_argument(arg, &mut args)?;
        }
        Ok(options)
    }
}

impl<'a> Options<'a> {
    fn apply_argument(&mut self, arg: &'a str, args: &mut impl Iterator<Item = &'a str>) -> Result<(), String> {
        let Some(option) = ValueOption::parse(arg) else {
            return switches::apply(self, arg);
        };
        let value = args.next().ok_or_else(|| option.missing(arg))?;
        option.apply(self, value, arg)
    }

    fn positional(&mut self, mut args: impl Iterator<Item = &'a str>) {
        if self.needs_pattern() {
            self.patterns.extend(args.next());
        }
        self.query.paths.extend(args);
    }

    fn needs_pattern(&self) -> bool {
        self.patterns.is_empty() && !self.list_types && !self.query.files && !self.pattern_file
    }

    pub(crate) fn caller_output(&self) -> Option<CallerOutput> {
        if self.list_types { return Some(CallerOutput::Types) }
        if self.query.empty_search() { return Some(CallerOutput::Empty) }
        self.query.reads_stdin().then_some(CallerOutput::Stdin)
    }

    pub(crate) fn requires_caller(&self) -> bool {
        self.query.requires_caller()
    }

    pub(crate) fn finish(mut self) -> Result<Lines<'a>, String> {
        let needs_pattern = self.needs_pattern();
        self.context.apply(&mut self.query);
        // Quiet asks whether any record was selected, in either flag order.
        self.query.apply_selection_mode();
        self.glob_specs.into_iter().try_for_each(|(value, insensitive)| {
            self.query.globs.add(value, insensitive || self.glob_case)
        })?;
        self.query.types = types::Types::build(self.edits, self.selections, self.list_types)?;
        if self.list_types {
            return Ok(self.query);
        }
        let sources = self.patterns.as_slice();
        if needs_pattern {
            return Err("missing pattern".into());
        }
        if self.query.empty_search() {
            return Ok(self.query);
        }
        self.query.replacement = self.replacement.map(super::replace::Template::new);
        let mut compiler =
            self.matcher.compiler(&self.query, sources);
        self.query.patterns = compiler.compile_all(sources)?;
        self.query.byte_input = compiler.byte_input;
        self.query.select_multiline_strategy();
        self.query.prepare_empty_matches();
        Ok(self.query)
    }
}

impl Lines<'_> {
    fn set_multiline(&mut self, enabled: bool) {
        self.multiline = enabled;
        self.stop_on_nonmatch &= !enabled;
    }

    fn set_stop_on_nonmatch(&mut self, enabled: bool) {
        self.stop_on_nonmatch = enabled;
        self.multiline &= !enabled;
    }

    fn apply_selection_mode(&mut self) {
        if self.quiet {
            self.result = ResultMode::Lines;
        }
    }

    fn select_multiline_strategy(&mut self) {
        if !self.multiline {
            return;
        }
        let separator = if self.null_data { '\0' } else { '\n' };
        self.multiline = self.patterns.iter().any(|pattern| pattern.needs_full_text(separator));
    }

    fn may_match_inside_scalar(&self) -> bool {
        let raw_display = self.only && self.replacement.is_none();
        let record_utf8 = !self.byte_input && !self.multiline;
        let unrestricted = self.match_scope == super::MatchScope::Anywhere;
        raw_display && record_utf8 && unrestricted
    }

    fn prepare_empty_matches(&mut self) {
        let mut closure = super::EpsilonClosure::default();
        if self.may_match_inside_scalar() {
            self.empty_inside_scalar = self.empty_match(super::EmptyPosition::InsideScalar, &mut closure);
        }
        let line_matching = !self.null_data && !self.multiline;
        if self.crlf && line_matching {
            self.crlf_edge = [false, true].map(|followed_by_lf|
                self.empty_match(super::EmptyPosition::AfterCarriageReturn { followed_by_lf }, &mut closure));
        }
    }
}
