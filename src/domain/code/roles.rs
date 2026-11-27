//! Classify source roles on demand against the pinned bytes of a file.
use super::{Kind, Language, Token, indentation, lexer, script};
mod suite_bodies;
pub(super) use registrations::RubyBlocks;
pub(super) use suite_bodies::suite_bodies;
mod asset_syntax;
pub(super) use asset_syntax::{WebHarness, harness_registrations};
mod build_roles;
mod c_family;
mod class_roles;
mod packaging;
mod registrations;
#[derive(Default)]
pub(crate) struct Context {
    headers: c_family::Headers,
    build: build_roles::BuildRoles,
    js_exports: javascript::ModuleExports,
    classes: class_roles::Classes,
    packaging: packaging::Packaging,
}
impl Context {
    fn registrations(
        &mut self,
        file: &RolePath,
        s: &Syntax,
        out: &mut Roles,
        read: &mut impl FnMut(&std::path::Path) -> std::io::Result<Option<String>>,
    ) -> std::io::Result<()> {
        match file.lang {
            Language::CMake => {
                self.build
                    .ctest
                    .registrations(std::path::Path::new(file.path), s, out, read)?
            }
            Language::Makefile => build_roles::make_targets(s.text, out),
            Language::Sql | Language::PlPgSql => self.build.sql(file.path, s, out, read)?,
            Language::Batchfile => asset_syntax::batch(file.path, s, out, read)?,
            _ => source_registrations(file, s, out),
        }
        Ok(())
    }

    pub fn new<'a>(paths: impl Iterator<Item = &'a str>) -> Self {
        let mut context = Self::default();
        context.packaging.scripts = paths
            .filter(|p| packaging::installer(p))
            .map(str::to_owned)
            .collect();
        context
    }
}
fn source_registrations(file: &RolePath, s: &Syntax, out: &mut Roles) {
    match file.lang {
        Language::Proto => asset_syntax::schema(file.path, s, out),
        Language::Nix => asset_syntax::nix(s, out),
        Language::Perl => asset_syntax::perl(s, out),
        _ => registrations::collect(file, s, out, &Default::default()),
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
    fn evidence(self, reason: &'static str) -> Evidence {
        Evidence(self, reason)
    }

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
impl Evidence {
    fn between(self, first: &Token, last: &Token) -> Region {
        Region {
            first: first.line,
            last: last.line + last.text.bytes().filter(|&b| b == b'\n').count(),
            start: first.span.start,
            end: last.span.end,
            evidence: self,
        }
    }
}

impl Region {
    fn in_host(mut self, byte_offset: usize, preceding_lines: usize) -> Self {
        self.start += byte_offset;
        self.end += byte_offset;
        self.first += preceding_lines;
        self.last += preceding_lines;
        self
    }
}

pub(crate) struct Roles {
    pub base: Evidence,
    pub regions: Vec<Region>,
}

fn test_directory(name: &str, lang: Option<Language>) -> bool {
    if name == "Test" && lang == Some(Language::Perl) {
        return false;
    }
    name.ends_with("Tests")
        || name == "t" && matches!(lang, Some(Language::Perl | Language::Assembly))
        || [
            "test",
            "tests",
            "__tests__",
            "spec",
            "specs",
            "unittests",
            "testfiles",
            "testdata",
            "__mocks__",
            "tck",
        ]
        .iter()
        .any(|label| name.eq_ignore_ascii_case(label))
}

fn ends_ascii_case(name: &str, suffix: &[u8]) -> bool {
    name.len()
        .checked_sub(suffix.len())
        .is_some_and(|start| name.as_bytes()[start..].eq_ignore_ascii_case(suffix))
}

fn class_test_suffix(lang: Option<Language>) -> bool {
    matches!(
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
    )
}

fn language_test_name(name: &str, stem: &str, lang: Option<Language>) -> bool {
    class_test_suffix(lang)
        && ["Test", "Tests", "TestCase"]
            .iter()
            .any(|suffix| stem.ends_with(suffix))
        || name.ends_with(".Tests.ps1")
        || match lang {
            Some(Language::Shell) => name.ends_with(".bats"),
            Some(Language::Perl) => name.ends_with(".t"),
            _ => false,
        }
}

fn test_filename(name: &str, stem: &str, lang: Option<Language>) -> bool {
    name.as_bytes().get(..5).is_some_and(|prefix| {
        prefix[..4].eq_ignore_ascii_case(b"test") && matches!(prefix[4], b'_' | b'-')
    }) || ["_test", "_spec", ".spec", ".test"]
        .iter()
        .any(|suffix| stem.ends_with(suffix))
        || language_test_name(name, stem, lang)
        || name == "conftest.py"
        || ends_ascii_case(name, b".tftest.hcl")
}

#[derive(Default)]
struct PathEvidence {
    test_dir: bool,
    fixture: bool,
    example: bool,
}

impl PathEvidence {
    fn observe(&mut self, part: &str, lang: Option<Language>) {
        self.test_dir = self.test_dir || test_directory(part, lang);
        self.fixture |= part == "fixtures";
        self.example |= matches!(part, "example" | "examples" | "demo" | "samples");
    }

    fn classify(&self, path: &str, name: &str, lang: Option<Language>) -> Evidence {
        if self.test_dir {
            return Evidence(Role::Support, "test directory");
        }
        if self.fixture && lang == Some(Language::Procfile) {
            return Evidence(Role::Support, "fixture application process definition");
        }
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        if test_filename(name, stem, lang) {
            return Evidence(Role::Support, "test filename");
        }
        if self.unsupported(path, lang) {
            return Evidence(Role::Unknown, "ambiguous or unsupported source");
        }
        Evidence(Role::Production, "ordinary source path")
    }

    fn unsupported(&self, path: &str, lang: Option<Language>) -> bool {
        let ambiguous = self.fixture || self.example;
        ambiguous || (lang.is_none() && !is_markdown(path))
    }
}

fn file_role(path: &str, lang: Option<Language>) -> Evidence {
    let mut parts = path.rsplit('/');
    let name = parts.next().unwrap_or(path);
    let mut child = name;
    let mut dirs = PathEvidence::default();
    for part in parts {
        if part == "conformance" && matches!(child, "cases" | "fixtures") {
            return Evidence(Role::Support, "conformance suite inputs");
        }
        dirs.observe(part, lang);
        child = part;
    }
    dirs.classify(path, name, lang)
}

#[derive(Clone, Copy)]
pub(super) enum RoleSyntax {
    PathOnly,
    Html,
    Header,
    Class,
    Javascript,
    Starlark,
    Registration,
}

pub(crate) fn needs_source(path: &str) -> bool {
    role_language(path, None)
        .is_some_and(|lang| !matches!(lang.descriptor().roles, RoleSyntax::PathOnly))
}

#[cfg(test)]
pub(crate) fn classify(path: &str, text: &str) -> Roles {
    classify_with(path, text, &mut Context::default(), |_| Ok(None)).unwrap()
}
fn role_language(path: &str, text: Option<&str>) -> Option<Language> {
    if ends_ascii_case(path, b".dockerfile") {
        Some(Language::Dockerfile)
    } else if path.ends_with(".bats") {
        Some(Language::Shell)
    } else if path.ends_with(".t")
        && (path.split('/').any(|p| p == "t")
            || text.is_none_or(super::score::suite_nesting::perl_source))
    {
        Some(Language::Perl)
    } else if path.ends_with(".php") && text.is_some_and(|s| s.trim_start().starts_with("<?hh")) {
        Some(Language::Hack)
    } else {
        Language::for_path(path)
    }
}

impl Context {
    fn base_role(
        &mut self,
        path: &str,
        lang: Option<Language>,
        read: &mut impl FnMut(&std::path::Path) -> std::io::Result<Option<String>>,
    ) -> std::io::Result<Evidence> {
        let base = file_role(path, lang);
        if self.packaging.applies(path, base, &mut *read)? {
            return Ok(Evidence(
                Role::Production,
                "packaged by an installation script",
            ));
        }
        if lang == Some(Language::Sql)
            && path.starts_with("t/")
            && self.build.perl_distribution(read)?
        {
            return Ok(Evidence(Role::Support, "Perl distribution test tree"));
        }
        Ok(base)
    }
}

pub(super) fn role_tokens(text: &str, lang: Language) -> Vec<Token<'_>> {
    if lang == Language::Ruby {
        super::extended::scripts::ruby_role_tokens(text)
    } else {
        lexer::tokens(text, lang)
    }
}

pub(crate) fn classify_with(
    path: &str,
    text: &str,
    context: &mut Context,
    mut read: impl FnMut(&std::path::Path) -> std::io::Result<Option<String>>,
) -> std::io::Result<Roles> {
    let lang = role_language(path, Some(text));
    let base = context.base_role(path, lang, &mut read)?;
    let mut result = Roles {
        base,
        regions: Vec::new(),
    };
    let Some(lang) = lang else { return Ok(result) };
    match lang.descriptor().roles {
        RoleSyntax::Html => {
            asset_syntax::html(text, &mut result);
            return Ok(result);
        }
        RoleSyntax::PathOnly => return Ok(result),
        _ => {}
    }
    let tokens = role_tokens(text, lang);
    let pairs = script::paired_for_score(&tokens);
    let syntax = Syntax {
        tokens: &tokens,
        pairs: &pairs,
        text,
    };
    RolePath::new(path, lang).collect_syntax(syntax, result, context, &mut read)
}

pub(super) fn on_test_path(path: &str, lang: Language) -> bool {
    file_role(path, Some(lang)).0 == Role::Support
}

impl Roles {
    /// Regions are ordered by source start, outermost first at a shared start.
    /// A caller doing lexical analysis can skip a nested declaration's body
    /// while preserving the enclosing test's state on either side.
    pub(super) fn tests_with_children(&self) -> impl Iterator<Item = (&Region, &[Region])> {
        self.regions.iter().enumerate().filter_map(|(i, region)| {
            if region.evidence.0 != Role::Test {
                return None;
            }
            let later = &self.regions[i + 1..];
            let end = later.partition_point(|child| child.start < region.end);
            Some((region, &later[..end]))
        })
    }
}

/// Lexical evidence for one path. Manifest-dependent classification uses Context.
pub(super) struct RolePath<'a> {
    pub(super) path: &'a str,
    lang: Language,
    evidence: std::cell::OnceCell<Evidence>,
}

impl<'a> RolePath<'a> {
    pub(super) fn new(path: &'a str, lang: Language) -> Self {
        Self {
            path,
            lang,
            evidence: std::cell::OnceCell::new(),
        }
    }

    fn evidence(&self) -> Evidence {
        *self
            .evidence
            .get_or_init(|| file_role(self.path, Some(self.lang)))
    }

    pub(super) fn is_support(&self) -> bool {
        self.evidence().0 == Role::Support
    }

    fn initial(&self) -> Roles {
        Roles {
            base: self.evidence(),
            regions: Vec::new(),
        }
    }

    fn collect_syntax(
        &self,
        syntax: Syntax,
        mut result: Roles,
        context: &mut Context,
        read: &mut impl FnMut(&std::path::Path) -> std::io::Result<Option<String>>,
    ) -> std::io::Result<Roles> {
        let (lang, path) = (self.lang, self.path);
        match lang.descriptor().roles {
            RoleSyntax::Javascript => {
                context
                    .js_exports
                    .collect(path, &syntax, &mut result, read)?
            }
            RoleSyntax::Class => context
                .classes
                .collect(self, &syntax, &mut result, &mut *read)?,
            RoleSyntax::Starlark => starlark::collect(&syntax, &mut result),
            RoleSyntax::Header => {
                context
                    .headers
                    .collect(path, &syntax, &mut result, &mut *read)?;
                context.build.ctest.entry_points(
                    std::path::Path::new(path),
                    &syntax,
                    &mut result,
                    read,
                )?;
            }
            _ => context.registrations(self, &syntax, &mut result, read)?,
        }
        result
            .regions
            .sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
        result
            .regions
            .dedup_by_key(|r| (r.start, r.end, r.evidence.0));
        Ok(result)
    }

    /// Reuse the query's tokenization for runner evidence available in this file.
    pub(super) fn local_regions(&self, text: &str, input: super::score::PairedSyntax) -> Roles {
        let result = self.initial();
        self.collect_syntax(
            Syntax {
                text,
                tokens: input.tokens,
                pairs: input.pairs,
            },
            result,
            &mut Context::default(),
            &mut |_| Ok(None),
        )
        .expect("file-local role collection performs no I/O")
    }

    pub(super) fn suite_regions(
        &self,
        text: &str,
        input: super::score::PairedSyntax,
        ruby: &std::cell::OnceCell<RubyBlocks>,
    ) -> Vec<Region> {
        let syntax = Syntax {
            tokens: input.tokens,
            pairs: input.pairs,
            text,
        };
        let mut roles = self.initial();
        if matches!(self.lang, Language::Sql | Language::PlPgSql) {
            if !build_roles::tap_regions(&syntax, &mut roles) {
                return Vec::new();
            }
        } else {
            registrations::collect(self, &syntax, &mut roles, ruby);
        }
        roles.regions.sort_by_key(|r| (r.start, r.end));
        roles.regions
    }
}

struct Syntax<'a> {
    tokens: &'a [Token<'a>],
    pairs: &'a [usize],
    text: &'a str,
}
fn next_arg(s: &Syntax, mut i: usize, end: usize) -> usize {
    while i < end && s.w(i) != "," {
        i = s.close(i).unwrap_or(i) + 1;
    }
    i
}

fn arguments<'a>(
    s: &'a Syntax,
    mut start: usize,
    end: usize,
) -> impl Iterator<Item = std::ops::Range<usize>> + 'a {
    std::iter::from_fn(move || {
        if start >= end {
            return None;
        }
        let close = next_arg(s, start, end);
        let arg = start..close;
        start = close + 1;
        Some(arg)
    })
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
    fn body(&self, i: usize) -> Option<usize> {
        let (at, token) = self.next_body_token(i, &["{", ";", "}"])?;
        (token == "{").then(|| self.close(at)).flatten()
    }
    fn kotlin_body(&self, i: usize) -> Option<usize> {
        let (function, token) = self.next_body_token(i, &["fun", ";", "}"])?;
        if token != "fun" {
            return None;
        }
        let (at, token) = self.next_body_token(function + 1, &["fun", "{", "=", ";", "}"])?;
        match token {
            "{" => self.close(at),
            "=" => self.kotlin_expression_end(at + 1),
            _ => None,
        }
    }
    fn next_body_token(&self, mut i: usize, wanted: &[&str]) -> Option<(usize, &str)> {
        loop {
            let token = self.tokens.get(i)?.text;
            if wanted.contains(&token) {
                return Some((i, token));
            }
            i = self.after_body_token(i, token)?;
        }
    }

    fn after_body_token(&self, i: usize, token: &str) -> Option<usize> {
        let last = if matches!(token, "(" | "[") {
            self.close(i)?
        } else {
            i
        };
        Some(last + 1)
    }

    fn kotlin_expression_end(&self, mut i: usize) -> Option<usize> {
        let mut end = self.tokens.get(i).map(|_| i)?;
        while i < self.tokens.len() && !matches!(self.w(i), ";" | "}") {
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
        Some(end)
    }
    fn indented(&self, i: usize) -> usize {
        let t = self.tokens;
        let base = indentation(self.text, t[i].span.start);
        let mut j = i + 1;
        while j < t.len() {
            if t[j].line > t[j - 1].line && indentation(self.text, t[j].span.start) <= base {
                break;
            }
            j = self.close(j).unwrap_or(j) + 1;
        }
        j - 1
    }
    fn add(&self, res: &mut Roles, i: usize, j: Option<usize>, evidence: Evidence) {
        if let Some(last) = j.and_then(|j| self.tokens.get(j)) {
            res.regions.push(evidence.between(&self.tokens[i], last));
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn distribution_evidence_belongs_to_a_role_query() {
    use std::{io, path::Path};
    for (manifest, probes) in [(Some("dist.ini"), 1), (Some("Build.PL"), 4), (None, 4)] {
        let mut context = Context::default();
        let mut reads = 0;
        let mut check = |path| {
            context
                .base_role(path, Some(Language::Sql), &mut |p: &Path| {
                    reads += 1;
                    Ok((p.to_str() == manifest).then(String::new))
                })
                .unwrap()
        };
        let first = check("t/one.sql");
        let second = check("t/two.sql");
        assert_eq!(first.1, second.1);
        assert_eq!(first.1 == "Perl distribution test tree", manifest.is_some());
        assert_eq!(reads, probes);
    }
    let mut context = Context::default();
    let failed = context.base_role("t/one.sql", Some(Language::Sql), &mut |_| {
        Err(io::Error::other("temporary read failure"))
    });
    assert!(failed.is_err());
    let role = context
        .base_role("t/one.sql", Some(Language::Sql), &mut |_| {
            Ok(Some(String::new()))
        })
        .unwrap();
    assert_eq!(role.1, "Perl distribution test tree");
}
