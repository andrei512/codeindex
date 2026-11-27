//! Follow simple function-like macro aliases in the source and its quoted local
//! headers. Header bodies stay in the pinned snapshot and never execute.
use super::{Language, Role, Roles, Syntax, Token, lexer, script};
use crate::domain::code::cfamily::directives::{Directive as Keyword, WrittenDirective};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Component, Path, PathBuf},
};

#[derive(Default)]
struct Defines {
    local: HashSet<String>,
    aliases: Vec<(String, String)>,
}

enum Directive<'a> {
    Define(usize),
    Include(&'a str),
}

fn directives<'t, 'a>(tokens: &'t [Token<'a>]) -> impl Iterator<Item = Directive<'a>> + 't {
    tokens.iter().enumerate().filter_map(move |(i, marker)| {
        if marker.text != "#" || continues_directive_line(tokens, i) {
            return None;
        }
        let row = tokens.get(i..i + 3)?;
        let keyword = WrittenDirective::read(row, false)?;
        let name = keyword
            .args
            .first()
            .filter(|name| name.line == marker.line)?;
        match keyword.kind {
            Keyword::Include if name.kind == super::Kind::String => {
                Some(Directive::Include(name.text))
            }
            Keyword::Write(true) => Some(Directive::Define(i + 2)),
            _ => None,
        }
    })
}

fn continues_directive_line(tokens: &[Token], i: usize) -> bool {
    let Some(prior) = i.checked_sub(1) else {
        return false;
    };
    let previous = &tokens[prior];
    let line = tokens[i].line;
    previous.line == line || escaped_line(previous, line)
}

fn escaped_line(previous: &Token, line: usize) -> bool {
    previous.text == "\\" && previous.line + 1 == line
}

fn alias_target<'a>(tokens: &[Token<'a>], close: usize) -> Option<&'a str> {
    let mut j = close + 1;
    while tokens.get(j).is_some_and(|t| t.text == "\\") {
        j += 1;
    }
    let [target, call] = tokens.get(j..j + 2)? else {
        return None;
    };
    let same_directive = target.line == tokens[close].line || j > close + 1;
    (same_directive && call.text == "(").then_some(target.text)
}

impl Defines {
    fn observe_macro(&mut self, tokens: &[Token], pairs: &[usize], name: usize) {
        let open = name + 1;
        let Some(token) = tokens.get(open) else {
            return;
        };
        if token.text != "(" || token.span.start != tokens[name].span.end {
            return;
        }
        let name = tokens[name].text;
        self.local.insert(name.to_owned());
        let close = pairs[open];
        if close >= tokens.len() {
            return;
        }
        if let Some(target) = alias_target(tokens, close) {
            self.aliases.push((name.to_owned(), target.to_owned()));
        }
    }
}

fn builtin(name: &str) -> bool {
    matches!(
        name,
        "TEST"
            | "TEST_F"
            | "TEST_P"
            | "TYPED_TEST"
            | "TEST_CASE"
            | "SCENARIO"
            | "TEST_IMPL"
            | "BOOST_AUTO_TEST_CASE"
            | "BOOST_FIXTURE_TEST_CASE"
            | "START_TEST"
    )
}
fn include_path(path: &str, spelling: &str) -> Option<PathBuf> {
    let rel = spelling.strip_prefix('"')?.strip_suffix('"')?;
    if Path::new(rel).is_absolute() {
        return None;
    }
    let mut key = PathBuf::new();
    for component in Path::new(path).parent()?.join(rel).components() {
        match component {
            Component::Normal(name) => key.push(name),
            Component::ParentDir if !key.pop() => return None,
            _ => {}
        }
    }
    (!key.as_os_str().is_empty()).then_some(key)
}

fn header_definitions(source: &str) -> Defines {
    let tokens = lexer::tokens(source, Language::Cpp);
    let pairs = script::paired_for_score(&tokens);
    let mut definitions = Defines::default();
    for directive in directives(&tokens) {
        if let Directive::Define(name) = directive {
            definitions.observe_macro(&tokens, &pairs, name);
        }
    }
    definitions
}

struct Registrations<'a> {
    local: &'a Defines,
    aliases: HashSet<&'a str>,
}

impl<'a> Registrations<'a> {
    fn new(local: &'a Defines, included: impl Iterator<Item = &'a Defines>) -> Self {
        let mut resolved = Self {
            local,
            aliases: HashSet::new(),
        };
        let mut users: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut pending = Vec::new();
        for (alias, target) in std::iter::once(local)
            .chain(included)
            .flat_map(|d| &d.aliases)
        {
            users.entry(target).or_default().push(alias);
            if resolved.contains(target) {
                pending.push(target.as_str());
            }
        }
        while let Some(target) = pending.pop() {
            let callers = users.remove(target).unwrap_or_default();
            resolved.aliases.extend(callers.iter().copied());
            pending.extend(callers);
        }
        resolved
    }

    fn contains(&self, name: &str) -> bool {
        builtin(name) && !self.local.local.contains(name) || self.aliases.contains(name)
    }

    fn body(&self, s: &Syntax, i: usize) -> Option<usize> {
        let previous = i.checked_sub(1).map(|prior| s.w(prior));
        if s.w(i + 1) != "(" || previous == Some("define") {
            return None;
        }
        let brace = s.close(i + 1)? + 1;
        (s.w(brace) == "{" && self.contains(s.w(i))).then_some(brace)
    }
}

#[derive(Default)]
pub(crate) struct Headers(HashMap<PathBuf, Defines>);
impl Headers {
    fn load(
        &mut self,
        path: &Path,
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<()> {
        if !self.0.contains_key(path) {
            let source = read(path)?;
            let defines = source
                .as_deref()
                .map(header_definitions)
                .unwrap_or_default();
            self.0.insert(path.to_owned(), defines);
        }
        Ok(())
    }

    fn include(
        &mut self,
        source: (&str, &str),
        headers: &mut HashSet<PathBuf>,
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<()> {
        if let Some(path) = include_path(source.0, source.1).filter(|path| !headers.contains(path))
        {
            self.load(&path, read)?;
            headers.insert(path);
        }
        Ok(())
    }

    pub(super) fn collect(
        &mut self,
        path: &str,
        s: &Syntax,
        res: &mut Roles,
        mut read: impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<()> {
        let mut local = Defines::default();
        let mut headers = HashSet::new();
        // Read first occurrences in source order; aliases may normalize to one header.
        for directive in directives(s.tokens) {
            match directive {
                Directive::Define(name) => local.observe_macro(s.tokens, s.pairs, name),
                Directive::Include(spelling) => {
                    self.include((path, spelling), &mut headers, &mut read)?;
                }
            }
        }
        let included = headers.iter().map(|key| &self.0[key]);
        let registrations = Registrations::new(&local, included);
        let bodies = (0..s.tokens.len()).filter_map(|i| Some((i, registrations.body(s, i)?)));
        for (i, brace) in bodies {
            s.add(
                res,
                i,
                s.close(brace),
                Role::Test.evidence("test registration macro"),
            );
        }
        Ok(())
    }
}
