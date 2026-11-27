//! Compile all pattern operands under the same matcher flags and size budget.
use crate::domain::pattern::{CompileOptions, MatchScope};
use super::{Lines, Pattern, Regex, pcre};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Engine {
    #[default]
    Native,
    Auto,
    Pcre,
}

impl Engine {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "default" => Ok(Self::Native),
            "auto" => Ok(Self::Auto),
            "pcre2" => Ok(Self::Pcre),
            _ => Err(format!("unknown regex engine: {value}")),
        }
    }
}

#[derive(Default)]
pub(super) enum Case {
    #[default]
    Sensitive,
    Insensitive,
    Smart,
}

impl Case {
    fn fold(&self, patterns: &[&str]) -> bool {
        match self {
            Self::Insensitive => true,
            Self::Sensitive => false,
            Self::Smart => patterns.iter().all(|p| !p.chars().any(char::is_uppercase)),
        }
    }
}


#[derive(Default)]
pub(super) struct MatcherOptions {
    pub engine: Engine,
    pub fixed: bool,
    pub case: Case,
    pub regex_limit: Option<u64>,
}

struct Budget {
    limit: u64,
    remaining: u64,
}

impl Budget {
    fn charge(&mut self, bytes: u64) -> Result<(), String> {
        self.remaining = self.remaining.checked_sub(bytes)
            .ok_or_else(|| format!("compiled regex exceeds size limit of {}", self.limit))?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum CompileRoute {
    Literal,
    NativeFixed,
    NativeRegex { fallback: bool },
    Pcre { quote: bool },
}

impl CompileRoute {
    fn select(options: &MatcherOptions, syntax: CompileOptions) -> Self {
        match (options.engine, options.fixed) {
            (Engine::Pcre, quote) => Self::Pcre { quote },
            (_, false) => Self::NativeRegex { fallback: options.engine == Engine::Auto },
            _ if !syntax.fold && syntax.scope == MatchScope::Anywhere => Self::Literal,
            _ => Self::NativeFixed,
        }
    }
}

struct RecordSyntax {
    flags: &'static str,
    dotall: bool,
    newline_terminator: bool,
}

impl RecordSyntax {
    fn select(query: &Lines<'_>) -> Self {
        let dotall = query.dotall && query.multiline;
        let flags = match (query.multiline, dotall, query.crlf, query.null_data) {
            (false, _, _, false) => "",
            (true, true, true, _) => "(?msR)",
            (true, true, false, _) => "(?ms)",
            (true, false, true, _) => "(?mR)",
            _ => "(?m)",
        };
        Self { flags, dotall, newline_terminator: !query.multiline && !query.null_data }
    }

    fn validate(&self, pattern: &Pattern<'_>, source: &str, fixed: bool) -> Result<(), String> {
        if !self.newline_terminator { return Ok(()); }
        let literal = if fixed { source } else { pattern.required_literal() };
        if literal.contains('\n') {
            return Err(Regex::LITERAL_NEWLINE_ERROR.into());
        }
        Ok(())
    }
}

pub(super) struct Compiler {
    route: CompileRoute,
    fixed: bool,
    syntax: CompileOptions,
    records: RecordSyntax,
    allow_nul: bool,
    buffer: String,
    budget: Option<Budget>,
    pub byte_input: bool,
}

impl MatcherOptions {
    pub(super) fn compiler(&self, query: &Lines<'_>, sources: &[&str]) -> Compiler {
        let syntax = CompileOptions {
            scope: query.match_scope,
            fold: self.case.fold(sources),
            ascii: query.ascii,
            crlf: query.crlf,
            capture: query.replacement.as_ref().is_some_and(|t| t.needs_groups()),
            exclude_newline: false,
        };
        Compiler {
            route: CompileRoute::select(self, syntax),
            fixed: self.fixed,
            syntax,
            records: RecordSyntax::select(query),
            allow_nul: query.text || query.binary || query.null_data,
            buffer: String::new(),
            budget: self.regex_limit.map(|limit| Budget { limit, remaining: limit }),
            byte_input: false,
        }
    }
}

impl Compiler {
    pub fn compile_all<'p>(&mut self, sources: &[&'p str]) -> Result<Vec<Pattern<'p>>, String> {
        if sources.len() <= 1 || matches!(sources, [left, right] if left != right) {
            return sources.iter().map(|s| self.compile(s)).collect();
        }
        let mut compiled = Vec::new();
        use std::collections::{HashMap, hash_map::Entry};
        let mut seen = HashMap::new();
        for &source in sources {
            match seen.entry(source) {
                Entry::Vacant(slot) => {
                    let (pattern, cost) = self.compile_costed(source)?;
                    slot.insert(cost);
                    compiled.push(pattern);
                }
                Entry::Occupied(slot) => {
                    if let Some(budget) = &mut self.budget {
                        budget.charge(*slot.get())?;
                    }
                }
            }
        }
        Ok(compiled)
    }

    pub fn compile<'p>(&mut self, source: &'p str) -> Result<Pattern<'p>, String> {
        self.compile_costed(source).map(|(pattern, _)| pattern)
    }

    fn compile_costed<'p>(&mut self, source: &'p str) -> Result<(Pattern<'p>, u64), String> {
        if !self.allow_nul && contains_nul_pattern(source, self.fixed) {
            return Err("pattern contains \"\\0\" but it is impossible to match\n\nConsider enabling text mode with the --text flag (or -a for short). Otherwise,\nbinary detection is enabled and matching a NUL byte is impossible.".into());
        }
        let (pattern, metered) = self.matcher(source)?;
        if !matches!(self.route, CompileRoute::Pcre { .. }) {
            self.records.validate(&pattern, source, self.fixed)?;
        }
        let cost = match (&mut self.budget, metered, &pattern) {
            (Some(budget), true, Pattern::Regex(regex)) => {
                let cost = regex.instruction_bytes() as u64;
                budget.charge(cost)?;
                cost
            }
            _ => 0,
        };
        Ok((pattern, cost))
    }

    fn matcher<'p>(&mut self, source: &'p str) -> Result<(Pattern<'p>, bool), String> {
        Ok(match self.route {
            CompileRoute::Literal => (Pattern::Literal(source), false),
            CompileRoute::NativeFixed => (Pattern::Regex(Regex::literal(source, self.syntax)?), self.syntax.fold),
            CompileRoute::Pcre { quote: true } => (self.pcre(&pcre::literal(source))?, false),
            CompileRoute::Pcre { quote: false } => (self.pcre(source)?, false),
            CompileRoute::NativeRegex { fallback } => self.compile_regex(source, fallback)?,
        })
    }

    fn compile_regex<'p>(&mut self, source: &'p str, fallback: bool) -> Result<(Pattern<'p>, bool), String> {
        // Folding already requires a compiled matcher regardless of syntax.
        let metered = self.syntax.fold || regex_syntax(source);
        if !metered && self.plain_literal(source) {
            return Ok((Pattern::Literal(source), false));
        }
        Ok((self.native(source, fallback)?, metered))
    }

    fn pcre<'p>(&self, source: &str) -> Result<Pattern<'p>, String> {
        pcre::Pcre::new(source, self.syntax, self.records.dotall).map(Pattern::Pcre)
    }

    fn plain_literal(&self, source: &str) -> bool {
        // Each ASCII literal byte would emit one Consume followed by a final Accept.
        self.syntax.scope == MatchScope::Anywhere
            && !self.syntax.capture
            && !source.is_empty()
            && source.len() < Regex::MAX_INSTRUCTIONS
            && source.is_ascii()
    }

    fn native<'p>(&mut self, source: &'p str, fallback: bool) -> Result<Pattern<'p>, String> {
        self.buffer.clear();
        self.buffer.push_str("(?:");
        if self.syntax.ascii {
            self.buffer.push_str("(?-u)");
        }
        self.buffer.push_str(self.records.flags);
        self.buffer.push_str(source);
        self.buffer.push(')');
        let options = CompileOptions {
            exclude_newline: self.records.newline_terminator && source.contains(['\n', '\\']),
            ..self.syntax
        };
        let mut bytes = false;
        let compiled = Regex::for_lines(&self.buffer, options, &mut bytes);
        self.byte_input |= bytes;
        compiled.map(Pattern::Regex).or_else(|error| {
            self.recover_native_error(source, error, fallback && !bytes)
        })
    }

    fn recover_native_error<'p>(&self, source: &'p str, error: String, fallback: bool) -> Result<Pattern<'p>, String> {
        if fallback && error != Regex::LITERAL_NEWLINE_ERROR {
            return self.pcre(source);
        }
        Err(error)
    }
}
fn regex_syntax(source: &str) -> bool {
    source.bytes().any(|b| br"\.^$|?*+()[]{}".contains(&b))
}

/// Raw NULs are forbidden in both modes. Regex mode also recognizes an
/// unescaped hex zero. A backslash can quote the following backslash.
fn contains_nul_pattern(source: &str, fixed: bool) -> bool {
    if fixed {
        return source.as_bytes().contains(&0);
    }
    let mut bytes = source.as_bytes();
    while let Some(at) = bytes.iter().position(|&b| matches!(b, 0 | b'\\')) {
        let tail = &bytes[at..];
        if tail.iter().take(2).any(|&byte| byte == 0) || tail.starts_with(br"\x00") {
            return true;
        }
        bytes = tail.get(2..).unwrap_or_default();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_patterns_share_a_matcher_and_keep_the_declared_size_budget() {
        let query = Lines::default();
        let compiler = |limit| MatcherOptions { regex_limit: limit, ..Default::default() }.compiler(&query, &[]);
        let source = "needle[0-9]";
        let Pattern::Regex(regex) = compiler(None).compile(source).unwrap() else { panic!() };
        let cost = regex.instruction_bytes() as u64;
        let sources = [source, "literal", source];
        let compiled = compiler(Some(cost * 2)).compile_all(&sources).unwrap();
        assert!(matches!(compiled.as_slice(), [Pattern::Regex(_), Pattern::Literal("literal")]));
        let rejected = compiler(Some(cost * 2 - 1)).compile_all(&sources);
        assert!(matches!(rejected, Err(message) if message.contains("compiled regex exceeds size limit")));
    }

    #[test]
    fn plain_regex_keeps_native_instruction_limit_and_capture_precedence() {
        let query = Lines::default();
        let mut compiler = MatcherOptions { regex_limit: Some(1), ..Default::default() }.compiler(&query, &[]);
        let fits = "a".repeat(Regex::MAX_INSTRUCTIONS - 1);
        assert!(matches!(compiler.compile(&fits), Ok(Pattern::Literal(_))));
        let too_long = fits + "a";
        assert!(matches!(compiler.compile(&too_long), Err(e) if e.contains("instruction limit")));
        let query = Lines {
            replacement: Some(super::super::replace::Template::new("$1")),
            ..Lines::default()
        };
        let mut compiler = MatcherOptions::default().compiler(&query, &[]);
        assert!(matches!(compiler.compile("a"), Ok(Pattern::Regex(_))));
    }
}
