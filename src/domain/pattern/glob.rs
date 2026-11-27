use super::{Regex, Scratch};
use std::{iter::Peekable, str::Chars};

#[derive(Debug)]
pub struct Glob(Matcher);

#[derive(Debug)]
enum Matcher {
    Exact(Box<str>),
    BasenameSuffix(Box<str>),
    Pattern { prefix: Box<str>, regex: Regex },
}

// Stop before syntax that can consume a different byte than the written one.
fn syntax(c: char) -> bool {
    matches!(c, '*' | '?' | '[' | '{' | '}' | '\\')
}
fn literal_prefix(pattern: &str) -> &str {
    &pattern[..pattern.find(syntax).unwrap_or(pattern.len())]
}
fn basename_suffix(pattern: &str) -> Option<&str> {
    let suffix = pattern.strip_prefix('*')?;
    (!suffix.contains(|c| c == '/' || syntax(c))).then_some(suffix)
}

fn literal(c: char, s: &mut String) {
    if c.is_ascii_alphanumeric() || c == '_' {
        s.push(c);
        return;
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in c.encode_utf8(&mut [0; 4]).bytes() {
        s.push_str("\\x");
        s.push(char::from(HEX[(byte >> 4) as usize]));
        s.push(char::from(HEX[(byte & 15) as usize]));
    }
}
fn character_class(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
) -> Result<(), String> {
    let negated = chars.next_if(|c| *c == '!' || *c == '^').is_some();
    out.push('[');
    if negated {
        out.push('^')
    }
    // A closing bracket or hyphen in first position is a literal.
    let first = chars.next().ok_or("unclosed glob character class")?;
    literal(first, out);
    loop {
        match chars.next().ok_or("unclosed glob character class")? {
            ']' => break,
            '-' if chars.peek() != Some(&']') => out.push('-'),
            value => literal(value, out),
        }
    }
    out.push(']');
    Ok(())
}

pub(crate) enum BasenameGlob {
    Exact(Box<str>),
    Extension(Box<str>),
    Other(Glob),
}

/// Translate glob operators into the byte regex consumed by the search VM.
struct GlobExpression {
    regex: String,
    alternatives: Vec<(usize, usize)>,
    boundary: bool,
}

enum Star {
    Segment,
    Recursive,
    Directories,
    EntirePath,
}

impl Star {
    fn read(chars: &mut Peekable<Chars<'_>>, boundary: bool, starts_path: bool) -> Self {
        let mut count = 1;
        while chars.next_if_eq(&'*').is_some() {
            count += 1;
        }
        if count != 2 || !boundary {
            return Self::Segment;
        }
        let slash = chars.next_if_eq(&'/').is_some();
        let end = chars.peek().is_none();
        match (slash, end, starts_path) {
            (true, true, true) => Self::EntirePath,
            (true, _, _) => Self::Directories,
            (false, true, _) => Self::Recursive,
            _ => Self::Segment,
        }
    }

    fn fragment(self) -> (&'static str, bool) {
        match self {
            Self::Segment => ("[^/]*", false),
            Self::Recursive => (".*", false),
            Self::Directories => ("(?:.*/)?", true),
            Self::EntirePath => (".*", true),
        }
    }
}

impl GlobExpression {
    const START: &'static str = "(?s)\\A(?:";

    fn compile(prefix: &str, syntax_tail: &str) -> Result<String, String> {
        let mut expr = Self {
            regex: Self::START.into(),
            alternatives: Vec::new(),
            boundary: prefix.is_empty() || prefix.ends_with('/'),
        };
        for c in prefix.chars() {
            literal(c, &mut expr.regex);
        }
        let mut chars = syntax_tail.chars().peekable();
        while let Some(c) = chars.next() {
            expr.token(c, &mut chars)?;
        }
        if !expr.alternatives.is_empty() {
            return Err("unclosed glob alternate".into());
        }
        expr.regex.push_str(")\\z");
        Ok(expr.regex)
    }

    fn token(&mut self, c: char, chars: &mut Peekable<Chars<'_>>) -> Result<(), String> {
        self.boundary = match c {
            '*' => self.star(chars),
            '?' => { self.regex.push_str("[^/]"); false }
            '[' => { character_class(chars, &mut self.regex)?; false }
            '{' | '}' | ',' => self.alternate(c)?,
            '\\' => {
                literal(chars.next().ok_or("trailing backslash in glob")?, &mut self.regex);
                false
            }
            _ => { literal(c, &mut self.regex); c == '/' }
        };
        Ok(())
    }

    fn alternate(&mut self, token: char) -> Result<bool, String> {
        match token {
            '{' => Ok(self.open()),
            '}' => self.close().map(|()| false),
            ',' => Ok(self.comma()),
            _ => unreachable!("non-alternation glob token"),
        }
    }

    fn open(&mut self) -> bool {
        let start = self.regex.len();
        self.regex.push_str("(?:");
        self.alternatives.push((start, self.regex.len()));
        true
    }

    fn comma(&mut self) -> bool {
        let Some((_, branch)) = self.alternatives.last_mut() else {
            literal(',', &mut self.regex);
            return false;
        };
        if self.regex.len() != *branch {
            self.regex.push('|');
            *branch = self.regex.len();
        }
        true
    }

    fn close(&mut self) -> Result<(), String> {
        let (start, branch) = self.alternatives.pop().ok_or("unmatched closing brace in glob")?;
        if branch == self.regex.len() {
            if branch == start + 3 {
                self.regex.truncate(start);
                return Ok(());
            }
            self.regex.pop();
        }
        self.regex.push(')');
        Ok(())
    }

    fn star(&mut self, chars: &mut Peekable<Chars<'_>>) -> bool {
        let star = Star::read(chars, self.boundary, self.regex == Self::START);
        let (fragment, boundary) = star.fragment();
        self.regex.push_str(fragment);
        boundary
    }
}

fn simple_matcher(pattern: &str, prefix: &str, fold: bool) -> Option<Matcher> {
    if fold {
        return None;
    }
    if pattern.len() == prefix.len() {
        return Some(Matcher::Exact(prefix.into()));
    }
    basename_suffix(pattern).map(|suffix| Matcher::BasenameSuffix(suffix.into()))
}

impl Glob {
    /// A single final extension can also be selected by a hash lookup.
    /// The caller supplies the basename, with no directory separators.
    pub(crate) fn for_basename(self) -> BasenameGlob {
        match self.0 {
            Matcher::Exact(key) => BasenameGlob::Exact(key),
            Matcher::BasenameSuffix(key) if key.starts_with('.') && !key[1..].contains('.') => {
                BasenameGlob::Extension(key)
            }
            _ => BasenameGlob::Other(self),
        }
    }

    pub fn new(pattern: &str) -> Result<Self, String> {
        Self::with_case(pattern, false)
    }
    pub(crate) fn with_case(pattern: &str, fold: bool) -> Result<Self, String> {
        if pattern.len() > 16_384 {
            return Err("glob is too long".into());
        }
        // Literal bytes need one instruction each in the regex encoding.
        // The 16 KiB glob limit keeps these forms under both regex limits.
        // Their syntax is complete once we have checked for metacharacters.
        let prefix = literal_prefix(pattern);
        let tail = &pattern[prefix.len()..];
        if let Some(matcher) = simple_matcher(pattern, prefix, fold) {
            return Ok(Self(matcher));
        }
        let out = GlobExpression::compile(prefix, tail)?;
        let prefix = if fold { "" } else { prefix };
        let mut regex = Regex::with_case_folding(&out, fold)?;
        // Glob regexes run on bytes represented as scalars. Only an ASCII
        // required literal can be compared directly with the UTF-8 path.
        if !regex.required.is_ascii() || prefix.contains(&regex.required) {
            regex.required.clear();
        }
        Ok(Self(Matcher::Pattern {
            prefix: prefix.into(),
            regex,
        }))
    }
    pub fn is_match(&self, path: &str) -> bool {
        self.matches_with(path, &mut Scratch::default())
    }

    pub(crate) fn matches_with(&self, path: &str, scratch: &mut Scratch) -> bool {
        match &self.0 {
            Matcher::Exact(literal) => path == literal.as_ref(),
            Matcher::BasenameSuffix(suffix) => {
                path.ends_with(suffix.as_ref()) && !path.contains('/')
            }
            Matcher::Pattern { prefix, regex } => {
                path.starts_with(prefix.as_ref())
                    && (regex.required.is_empty() || path.contains(&regex.required))
                    && {
                        scratch.prepare(regex.program.len());
                        scratch.find::<true>(regex, path, 0).is_some()
                    }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_prefilters_preserve_byte_automaton_matches() {
        for pattern in [
            "*README*-spec*",
            "src/*.{ts,rs}",
            "*é?README",
            "[a-z]*xyz",
            r"*\{*.md",
            "*{a,b}*ç",
            "ß*/[Aa]",
        ] {
            for fold in [false, true] {
                let glob = Glob::with_case(pattern, fold).unwrap();
                let Matcher::Pattern { regex, .. } = &glob.0 else {
                    panic!("expected wildcard")
                };
                for path in [
                    "aREADME-spec",
                    "aREADMEb-specq",
                    "src/lib.rs",
                    "xé!README",
                    "axyz",
                    "{hello.md",
                    "abç",
                    "ß/a",
                    "missing",
                    "src/x.py",
                ] {
                    let expected = Scratch::new(regex.program.len())
                        .find::<true>(regex, path, 0)
                        .is_some();
                    assert_eq!(glob.is_match(path), expected, "{pattern:?} {path:?} {fold}");
                }
            }
        }
    }

    #[test]
    fn literal_and_suffix_forms_observe_the_glob_byte_limit() {
        let literal = "é".repeat(8192);
        assert!(Glob::new(&literal).unwrap().is_match(&literal));
        let suffix = "*".to_owned() + &"a".repeat(16383);
        let glob = Glob::new(&suffix).unwrap();
        assert!(glob.is_match(&suffix[1..]));
        assert!(!glob.is_match(&format!("dir/{}", &suffix[1..])));
        assert_eq!(Glob::new(&(literal + "x")).unwrap_err(), "glob is too long");
        assert!(Glob::new("*.c[").is_err());
        assert!(
            Glob::with_case("*.TXT", true)
                .unwrap()
                .is_match("guide.txt")
        );
    }

    #[test]
    fn an_empty_alternative_is_not_a_separator_before_double_star() {
        let pattern = Glob::new("a{}**/x").unwrap();
        assert!(pattern.is_match("abc/x"));
        assert!(!pattern.is_match("ax"));
        assert!(!pattern.is_match("a/b/x"));
    }
}
