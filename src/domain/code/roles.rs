//! Classify source roles on demand against the pinned bytes of a file.
use super::{Kind, Language, Token, indentation, lexer, script};
mod asset_syntax;
mod build_roles;
mod c_family;
mod class_roles;
mod packaging;
mod registrations;
#[derive(Default)]
pub(crate) struct Context {
    headers: c_family::Headers,
    classes: class_roles::Classes,
    packaging: packaging::Packaging,
}
impl Context {
    pub fn new<'a>(paths: impl Iterator<Item = &'a str>) -> Self {
        let mut context = Self::default();
        context.packaging.scripts = paths
            .filter(|p| packaging::installer(p))
            .map(str::to_owned)
            .collect();
        context
    }
}
mod javascript;
mod python;
mod starlark;
mod suites;
use crate::domain::markdown::is_markdown;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Test,
    Support,
    Production,
    Unknown,
}
impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Support => "test-support",
            Self::Production => "production",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) struct Evidence(pub Role, pub &'static str);
pub(crate) struct Region {
    pub first: usize,
    pub last: usize,
    pub start: usize,
    pub end: usize,
    pub evidence: Evidence,
}
pub(crate) struct Roles {
    pub base: Evidence,
    pub regions: Vec<Region>,
}

fn file_role(path: &str, lang: Option<Language>) -> Evidence {
    let mut parts = path.rsplit('/');
    let name = parts.next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let lower = name.to_ascii_lowercase();
    if path
        .split('/')
        .zip(path.split('/').skip(1))
        .any(|(a, b)| a == "conformance" && matches!(b, "cases" | "fixtures"))
    {
        return Evidence(Role::Support, "conformance suite inputs");
    }
    if parts.clone().any(|p| {
        (p != "Test" || lang != Some(Language::Perl))
            && matches!(
                p.to_ascii_lowercase().as_str(),
                "test"
                    | "tests"
                    | "__tests__"
                    | "spec"
                    | "specs"
                    | "unittests"
                    | "testfiles"
                    | "testdata"
                    | "__mocks__"
                    | "tck"
            )
            || p.ends_with("Tests")
            || p.ends_with(".Tests")
            || (p == "t" && matches!(lang, Some(Language::Perl | Language::Assembly)))
    }) {
        return Evidence(Role::Support, "test directory");
    }
    if lang == Some(Language::Procfile) && parts.clone().any(|d| d == "fixtures") {
        return Evidence(Role::Support, "fixture application process definition");
    }
    let class_convention = matches!(
        lang,
        Some(
            Language::Java
                | Language::Kotlin
                | Language::Swift
                | Language::ObjectiveC
                | Language::Csharp
                | Language::Php
                | Language::Hack
        )
    );
    if lower.starts_with("test_")
        || lower.starts_with("test-")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
        || stem.ends_with(".spec")
        || stem.ends_with(".test")
        || class_convention
            && (stem.ends_with("Test") || stem.ends_with("Tests") || stem.ends_with("TestCase"))
        || name.ends_with(".Tests.ps1")
        || (lang == Some(Language::Shell) && name.ends_with(".bats"))
        || (lang == Some(Language::Perl) && name.ends_with(".t"))
        || name == "conftest.py"
        || lower.ends_with(".tftest.hcl")
    {
        return Evidence(Role::Support, "test filename");
    }
    if lang.is_none() && !is_markdown(path)
        || parts.any(|p| matches!(p, "example" | "examples" | "demo" | "fixtures" | "samples"))
    {
        return Evidence(Role::Unknown, "ambiguous or unsupported source");
    }
    Evidence(Role::Production, "ordinary source path")
}

pub(crate) fn needs_source(path: &str) -> bool {
    path.ends_with(".t")
        || path.ends_with(".bats")
        || Language::for_path(path).is_some_and(registrations::has_syntax)
}

#[cfg(test)]
pub(crate) fn classify(path: &str, text: &str) -> Roles {
    classify_with(path, text, &mut Context::default(), |_| Ok(None)).unwrap()
}
pub(crate) fn classify_with(
    path: &str,
    text: &str,
    context: &mut Context,
    mut read: impl FnMut(&std::path::Path) -> std::io::Result<Option<String>>,
) -> std::io::Result<Roles> {
    let lang = if path.to_ascii_lowercase().ends_with(".dockerfile") {
        Some(Language::Dockerfile)
    } else if path.ends_with(".bats") {
        Some(Language::Shell)
    } else if path.ends_with(".t")
        && (path.split('/').any(|p| p == "t")
            || text.starts_with("#!") && text.lines().next().is_some_and(|l| l.contains("perl"))
            || text.lines().any(|l| l.starts_with("use Test::")))
    {
        Some(Language::Perl)
    } else if path.ends_with(".php") && text.trim_start().starts_with("<?hh") {
        Some(Language::Hack)
    } else {
        Language::for_path(path)
    };
    let mut base = file_role(path, lang);
    if context.packaging.applies(path, base, &mut read)? {
        base = Evidence(Role::Production, "packaged by an installation script");
    } else if lang == Some(Language::Sql) && path.starts_with("t/") {
        for manifest in ["dist.ini", "Makefile.PL", "cpanfile", "Build.PL"] {
            if read(std::path::Path::new(manifest))?.is_some() {
                base = Evidence(Role::Support, "Perl distribution test tree");
                break;
            }
        }
    }

    let mut result = Roles {
        base,
        regions: Vec::new(),
    };
    let Some(lang) = lang else { return Ok(result) };
    if lang == Language::Html {
        asset_syntax::html(text, &mut result);
        return Ok(result);
    }
    if !registrations::has_syntax(lang) {
        return Ok(result);
    }
    let tokens = lexer::tokens(text, lang);
    let pairs = script::paired_for_score(&tokens);
    let syntax = Syntax {
        tokens: &tokens,
        pairs: &pairs,
        text,
    };
    collect_syntax(lang, path, syntax, result, context, &mut read)
}

/// Reuse the query's tokenization for runner evidence available in this file.
pub(super) fn local_regions(lang: Language, path: &str, text: &str, tokens: &[Token], pairs: &[usize]) -> Roles {
    let result = Roles { base: file_role(path, Some(lang)), regions: Vec::new() };
    collect_syntax(lang, path, Syntax { text, tokens, pairs }, result, &mut Context::default(), &mut |_| Ok(None))
        .expect("file-local role collection performs no I/O")
}

fn collect_syntax(
    lang: Language, path: &str, syntax: Syntax, mut result: Roles, context: &mut Context,
    read: &mut impl FnMut(&std::path::Path) -> std::io::Result<Option<String>>,
) -> std::io::Result<Roles> {
    if lang == Language::Js {
        javascript::collect(path, &syntax, &mut result, read)?;
    } else if matches!(lang, Language::Java | Language::Swift | Language::Hack) {
        context
            .classes
            .collect(lang, path, &syntax, &mut result, &mut *read)?;
    } else if lang == Language::Starlark {
        starlark::collect(&syntax, &mut result);
    } else if matches!(lang, Language::C | Language::Cpp) {
        context
            .headers
            .collect(path, &syntax, &mut result, &mut *read)?;
    } else {
        registrations::collect(lang, path, &syntax, &mut result);
    }
    build_roles::collect(lang, path, &syntax, &mut result, read)?;
    asset_syntax::collect(lang, path, &syntax, &mut result, read)?;
    result
        .regions
        .sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    result
        .regions
        .dedup_by_key(|r| (r.start, r.end, r.evidence.0));
    Ok(result)
}

pub(super) fn on_test_path(path: &str, lang: Language) -> bool {
    file_role(path, Some(lang)).0 == Role::Support
}

pub(super) fn suite_regions(lang: Language, path: &str, text: &str, tokens: &[Token], pairs: &[usize]) -> Vec<Region> {
    let syntax = Syntax { tokens, pairs, text };
    let mut roles = Roles { base: file_role(path, Some(lang)), regions: Vec::new() };
    if matches!(lang, Language::Sql | Language::PlPgSql) {
        if !build_roles::tap_regions(&syntax, &mut roles) { return Vec::new(); }
    } else {
        registrations::collect(lang, path, &syntax, &mut roles);
    }
    roles.regions.sort_by_key(|r| (r.start, r.end));
    roles.regions
}

struct Syntax<'a> {
    tokens: &'a [Token<'a>],
    pairs: &'a [usize],
    text: &'a str,
}
impl Syntax<'_> {
    fn w(&self, i: usize) -> &str {
        self.tokens.get(i).map_or("", |t| t.text)
    }
    fn seq(&self, i: usize, words: &[&str]) -> bool {
        self.tokens
            .get(i..i.saturating_add(words.len()))
            .is_some_and(|s| s.iter().zip(words).all(|(t, w)| t.text == *w))
    }
    fn close(&self, i: usize) -> Option<usize> {
        self.pairs
            .get(i)
            .copied()
            .filter(|&j| j > i && j < self.tokens.len())
    }
    fn string(&self, i: usize) -> bool {
        self.tokens.get(i).is_some_and(|t| t.kind == Kind::String)
    }
    // Skip balanced argument lists so an annotation argument or a default value
    // cannot be mistaken for the declaration body.
    fn body(&self, mut i: usize) -> Option<usize> {
        while i < self.tokens.len() {
            match self.w(i) {
                "{" => return self.close(i),
                ";" | "}" => return None,
                "(" | "[" => i = self.close(i)?,
                _ => {}
            }
            i += 1;
        }
        None
    }
    fn kotlin_body(&self, mut i: usize) -> Option<usize> {
        let mut function = false;
        while i < self.tokens.len() {
            match self.w(i) {
                "fun" if function => return None,
                "fun" => function = true,
                "(" | "[" => i = self.close(i)?,
                "{" if function => return self.close(i),
                "=" if function => {
                    i += 1;
                    let mut end = self.tokens.get(i).map(|_| i)?;
                    while i < self.tokens.len() {
                        if self.w(i) == ";" || self.w(i) == "}" {
                            break;
                        }
                        if i > end
                            && self.tokens[i].line > self.tokens[end].line
                            && !matches!(self.w(i), "." | "?")
                            && !matches!(self.w(end), "=" | "+" | "-" | "." | "?")
                        {
                            break;
                        }
                        end = self.close(i).unwrap_or(i);
                        i = end + 1;
                    }
                    return Some(end);
                }
                ";" | "}" => return None,
                _ => {}
            }
            i += 1
        }
        None
    }
    fn indented(&self, i: usize) -> usize {
        let t = self.tokens;
        let base = indentation(self.text, t[i].span.start);
        let mut j = i + 1;
        while j < t.len() {
            if t[j].line > t[j - 1].line && indentation(self.text, t[j].span.start) <= base {
                break;
            }
            if let Some(end) = self.close(j) {
                j = end;
            }
            j += 1;
        }
        j - 1
    }
    fn add(&self, res: &mut Roles, i: usize, j: Option<usize>, role: Role, reason: &'static str) {
        let Some(last) = j.and_then(|j| self.tokens.get(j)) else {
            return;
        };
        let first = &self.tokens[i];
        res.regions.push(Region {
            first: first.line,
            last: last.line + last.text.bytes().filter(|&b| b == b'\n').count(),
            start: first.span.start,
            end: last.span.end,
            evidence: Evidence(role, reason),
        });
    }
}

#[cfg(test)]
mod tests;
