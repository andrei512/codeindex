//! Recognize test registration calls at the binding used by the caller.
use super::{Kind, Role, Roles, Syntax, arguments, next_arg};
use crate::domain::code::script::ExpressionBodies;
use std::{
    collections::{HashMap, HashSet},
    io,
    ops::Range,
    path::Path,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Binding {
    Other,
    Case,
    Suite,
    Hook,
    Namespace,
    NodeDefault,
    PlaywrightCase,
}

/// Classify the module once per import. Several runners share the named
/// registration API but supply different default exports.
#[derive(Clone, Copy)]
enum Runner {
    Node,
    Playwright,
    Ava,
    Mocha,
    Named,
    Unknown,
}

impl Runner {
    fn from_module(module: &str) -> Self {
        match module {
            "node:test" => Self::Node,
            "@playwright/test" => Self::Playwright,
            "ava" => Self::Ava,
            "mocha" => Self::Mocha,
            "bun:test" | "vitest" | "@jest/globals" => Self::Named,
            _ => Self::Unknown,
        }
    }

    fn export(self, name: &str) -> Binding {
        use Binding::*;
        match (self, name) {
            (Self::Unknown, _) => Other,
            (_, "*") | (Self::Mocha, "default") => Namespace,
            (Self::Node, "default") => NodeDefault,
            (Self::Playwright, "default" | "test") => PlaywrightCase,
            (Self::Ava, "default") => Case,
            _ => Binding::named_export(name),
        }
    }

    fn require(self) -> Binding {
        match self {
            Self::Node | Self::Ava => self.export("default"),
            _ => self.export("*"),
        }
    }
}

fn literal<'a>(s: &'a Syntax, i: usize) -> &'a str {
    let w = s.w(i);
    if w.len() >= 2
        && matches!(w.as_bytes()[0], b'\'' | b'"')
        && w.as_bytes().last() == w.as_bytes().first()
    {
        &w[1..w.len() - 1]
    } else {
        ""
    }
}

fn ident(s: &Syntax, i: usize) -> bool {
    s.tokens
        .get(i)
        .is_some_and(|t| t.kind == Kind::Word && !t.text.starts_with(|c: char| c.is_ascii_digit()))
}

mod bindings;
mod imports;
mod modules;
use bindings::{Resolved, ScopedBindings};
pub(super) use modules::ModuleExports;
use modules::Modules;

#[derive(Default)]
struct Scopes<'a> {
    relative: Vec<(&'a str, &'a str, &'a str)>,
    names: HashMap<&'a str, ScopedBindings>,
    file_end: usize,
    track_exports: bool,
    locals: Vec<(&'a str, Range<usize>, Binding)>,
}

impl<'a> Scopes<'a> {
    fn from_syntax(s: &'a Syntax) -> Self {
        Self::from_names(s, [], false)
    }

    fn from_names(
        s: &'a Syntax,
        globals: impl IntoIterator<Item = &'a str>,
        track_exports: bool,
    ) -> Self {
        let mut scopes = Self {
            names: globals
                .into_iter()
                .map(|name| (name, ScopedBindings::default()))
                .collect(),
            file_end: s.tokens.len(),
            track_exports,
            ..Self::default()
        };
        declarations(s, &mut scopes);
        scopes
    }

    fn with_locals(mut self) -> Self {
        // A local declaration can shadow an import written later in the file.
        for (name, scope, binding) in std::mem::take(&mut self.locals) {
            if let Some(declared) = self.names.get_mut(name) {
                declared.bind_local(scope, binding);
            }
        }
        self
    }

    fn bind(&mut self, scope: Range<usize>, name: &'a str, binding: Binding) {
        if scope == (0..self.file_end) {
            self.bind_file(name, binding);
        } else if !scope.is_empty() {
            self.tracked(name, binding);
            self.locals.push((name, scope, binding));
        }
    }

    fn tracked(&mut self, name: &'a str, binding: Binding) -> Option<&mut ScopedBindings> {
        if binding.relevant(name) {
            Some(self.names.entry(name).or_default())
        } else {
            self.names.get_mut(name)
        }
    }
    fn bind_file(&mut self, name: &'a str, binding: Binding) {
        if self.track_exports {
            self.names.entry(name).or_default().file = Some(binding);
        } else if let Some(bindings) = self.tracked(name, binding) {
            bindings.file = Some(binding);
        }
    }
    fn resolve(&self, at: usize, name: &str) -> Resolved {
        self.names
            .get(name)
            .map_or(Resolved::Absent, |b| b.at(at, self.file_end))
    }

    fn resolve_relative(
        &mut self,
        s: &Syntax,
        path: &Path,
        exports: &mut ModuleExports,
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<()> {
        let mut imports = std::mem::take(&mut self.relative);
        imports::retain_used(s, &mut imports);
        let mut modules = Modules::new(exports);
        for group in imports.chunk_by(|left, right| left.0 == right.0) {
            modules.resolve_imports(path, group, read, &mut |_, local, binding| {
                if !matches!(binding, Binding::Other) {
                    self.bind_file(local, binding);
                }
            })?;
        }
        Ok(())
    }
    fn registrations(&self, s: &Syntax, globals: bool, res: &mut Roles) {
        let registrations = call_candidates(s).filter_map(|head| {
            let binding = self.registration_binding(head, globals);
            let (end, role) = head.registration(s, binding)?;
            Some((head.index, end, role))
        });
        for (i, end, role) in registrations {
            s.add(
                res,
                i,
                Some(end),
                role.evidence("test framework registration"),
            );
        }
    }

    fn registration_binding(&self, head: CallHead, globals: bool) -> Binding {
        self.resolve(head.index, head.name)
            .binding()
            .unwrap_or_else(|| {
                if globals {
                    Runner::Mocha.export(head.name)
                } else {
                    Binding::Other
                }
            })
    }

    fn import(&mut self, clause: imports::Clause<'a, '_>) {
        let relative = clause.module.starts_with("./") || clause.module.starts_with("../");
        let runner = Runner::from_module(clause.module);
        for &(local, original) in clause.names {
            if relative {
                self.relative.push((clause.module, original, local));
            }
            self.bind_file(local, runner.export(original));
        }
    }
}

/// A bare identifier that can introduce a direct or member call.
#[derive(Clone, Copy)]
struct CallHead<'a> {
    index: usize,
    name: &'a str,
    member: Option<&'a str>,
}

impl<'a> CallHead<'a> {
    fn read(s: &'a Syntax, i: usize) -> Option<Self> {
        let member = match s.w(i + 1) {
            "(" => None,
            "." => Some(s.w(i + 2)),
            _ => return None,
        };
        let declaration_or_member = i
            .checked_sub(1)
            .is_some_and(|previous| matches!(s.w(previous), "." | "function" | "class"));
        if !ident(s, i) || declaration_or_member {
            return None;
        }
        Some(Self {
            index: i,
            name: s.w(i),
            member,
        })
    }

    fn direct_end(self, s: &Syntax) -> Option<usize> {
        self.member
            .is_none()
            .then(|| s.close(self.index + 1))
            .flatten()
    }

    fn relative(self, s: &Syntax) -> bool {
        // A namespace import can expose a registration as a member. Resolution
        // determines whether the local binding actually provides that member.
        let member = self
            .member
            .is_some_and(|name| Runner::Node.export(name) != Binding::Other);
        let at = self.index + 1 + 2 * usize::from(member);
        Call::parse(s, at).is_some_and(|call| call.accepts(s, false))
    }
}

fn call_candidates<'a>(s: &'a Syntax) -> impl Iterator<Item = CallHead<'a>> + 'a {
    (0..s.tokens.len()).filter_map(|i| CallHead::read(s, i))
}

pub(super) fn web_harness_calls(s: &Syntax, mut visit: impl FnMut(usize, usize)) {
    const GLOBALS: [&str; 4] = ["test", "async_test", "promise_test", "subsetTest"];
    let mut calls = call_candidates(s)
        .filter(|head| GLOBALS.contains(&head.name))
        .filter_map(|head| Some((head, head.direct_end(s)?)))
        .peekable();
    if calls.peek().is_none() {
        return;
    }
    let bindings = Scopes::from_names(s, GLOBALS, false).with_locals();
    for (head, close) in calls {
        if matches!(bindings.resolve(head.index, head.name), Resolved::Absent) {
            visit(head.index, close);
        }
    }
}

fn callable(s: &Syntax, start: usize, end: usize) -> bool {
    let Some(start) = (start..end).find(|&i| s.w(i) != "async") else {
        return false;
    };
    match s.w(start) {
        "function" => true,
        "(" => arrow_after_parameters(s, start, end),
        "null" | "false" | "true" | "undefined" => false,
        _ => ident(s, start) && callback_reference(s, start, end),
    }
}

// Identifier/member references can refer to callbacks declared elsewhere.
fn callback_reference(s: &Syntax, start: usize, end: usize) -> bool {
    let member = |i| i + 1 < end && s.w(i) == "." && ident(s, i + 1);
    let mut i = start + 1;
    while member(i) {
        i += 2;
    }
    i == end || s.seq(i, &["=", ">"])
}

fn arrow_after_parameters(s: &Syntax, start: usize, end: usize) -> bool {
    let Some(close) = s.close(start) else {
        return false;
    };
    match s.w(close + 1) {
        ":" => (close + 2..end).any(|i| s.seq(i, &["=", ">"])),
        "=" => s.w(close + 2) == ">",
        _ => false,
    }
}

mod declarations;
use declarations::declarations;

impl Binding {
    fn named_export(name: &str) -> Self {
        match name {
            "test" | "it" | "specify" => Self::Case,
            "describe" | "suite" | "context" => Self::Suite,
            "before" | "after" | "beforeEach" | "afterEach" | "beforeAll" | "afterAll" => {
                Self::Hook
            }
            _ => Self::Other,
        }
    }

    fn relevant(self, name: &str) -> bool {
        self != Self::Other || Runner::Mocha.export(name) != Self::Other
    }

    fn member(self, name: &str) -> Option<Self> {
        match self {
            Self::Namespace => Some(Runner::Node.export(name)),
            Self::NodeDefault => {
                let member = Runner::Node.export(name);
                (!matches!(member, Self::Other)).then_some(member)
            }
            Self::PlaywrightCase => match name {
                "describe" => Some(Self::Suite),
                "beforeEach" | "afterEach" | "beforeAll" | "afterAll" => Some(Self::Hook),
                _ => None,
            },
            _ => None,
        }
    }
}

impl CallHead<'_> {
    fn registration(self, s: &Syntax, mut binding: Binding) -> Option<(usize, Role)> {
        if binding == Binding::Other {
            return None;
        }
        let mut at = self.index + 1;
        if let Some(member) = self.member.and_then(|name| binding.member(name)) {
            binding = member;
            at += 2;
        }
        let role = match binding {
            Binding::Hook | Binding::Suite => Role::Support,
            Binding::Case | Binding::PlaywrightCase | Binding::NodeDefault => Role::Test,
            Binding::Namespace | Binding::Other => return None,
        };
        let call = Call::parse(s, at)?;
        call.accepts(s, binding == Binding::Hook)
            .then_some((call.end, role))
    }
}

struct Call {
    open: usize,
    end: usize,
    todo: bool,
}

impl Call {
    fn parse(s: &Syntax, mut at: usize) -> Option<Self> {
        let mut todo = false;
        while s.w(at) == "." {
            match s.w(at + 1) {
                "each" | "for" if s.w(at + 2) == "(" => at = s.close(at + 2)? + 1,
                "skip" | "only" | "concurrent" | "serial" | "fails" | "failing" => at += 2,
                "todo" => {
                    todo = true;
                    at += 2;
                }
                _ => return None,
            }
        }
        if s.w(at) != "(" {
            return None;
        }
        let end = s.close(at)?;
        // A following body makes this a method declaration.
        (s.w(end + 1) != "{").then_some(Self {
            open: at,
            end,
            todo,
        })
    }

    fn accepts(&self, s: &Syntax, hook: bool) -> bool {
        let mut args = arguments(s, self.open + 1, self.end);
        let Some(first) = args.next() else {
            return false;
        };
        if hook || !s.string(first.start) {
            callable(s, first.start, first.end)
        } else {
            self.todo || args.any(|a| callable(s, a.start, a.end))
        }
    }
}

// Outside a test directory, registrations require an ES import or CommonJS
// binding. An arbitrary string containing a runner name cannot supply one.
fn imports_runner(s: &Syntax) -> bool {
    s.tokens.iter().enumerate().any(|(i, token)| {
        if token.kind != Kind::String {
            return false;
        }
        let esm = i > 0 && s.w(i - 1) == "from";
        let common = i >= 2 && s.seq(i - 2, &["require", "("]);
        if !esm && !common {
            return false;
        }
        let module = literal(s, i);
        esm && module.starts_with('.') || !matches!(Runner::from_module(module), Runner::Unknown)
    })
}

mod wrappers;

impl ModuleExports {
    pub(super) fn collect(
        &mut self,
        path: &str,
        s: &Syntax,
        res: &mut Roles,
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<()> {
        let globals = res.base.0 == Role::Support;
        if !globals && !imports_runner(s) {
            return Ok(());
        }
        let mut bindings = Scopes::from_syntax(s);
        bindings.resolve_relative(s, Path::new(path), self, read)?;
        bindings.with_locals().registrations(s, globals, res);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::code::roles::{Role, classify};

    fn check(path: &str, source: &str, markers: &[(&str, Role)]) {
        let actual = classify(path, source);
        for &(marker, expected) in markers {
            let at = source.find(marker).unwrap();
            let role = actual
                .regions
                .iter()
                .rev()
                .find(|r| r.start <= at && at < r.end)
                .map_or(actual.base.0, |r| r.evidence.0);
            assert_eq!(role, expected, "{marker}");
        }
    }

    #[test]
    fn imported_bindings_and_local_names_are_distinct() {
        let src = r#"import {test as check} from 'node:test';
const test = (name, count) => name.length + count;
test('customer', 7);
check('registered', () => {});
function render(check: Callback): void {
    check('ordinary', () => {});
}
check('second case', {}, () => {});
"#;
        check(
            "src/components.ts",
            src,
            &[
                ("customer", Role::Production),
                ("registered", Role::Test),
                ("ordinary", Role::Production),
                ("second case", Role::Test),
            ],
        );
    }

    #[test]
    fn parameters_shadow_later_framework_imports_in_their_own_body() {
        check(
            "src/example.ts",
            "function early({run: verify}) { verify('shadowed', () => {}); }
verify('actual', () => {});
import {it as verify} from 'vitest';",
            &[("shadowed", Role::Production), ("actual", Role::Test)],
        );
    }

    #[test]
    fn global_bdd_and_data_driven_cases_observe_scope() {
        let src = r#"describe('suite', function() {
  beforeEach(() => prepare());
  it.each([[1],[2]])('registered case', (value) => verify(value));
  { const it = record; it('shadowed', () => local()); }
});
class Utility { it(label, handler) { return handler(label); } }
it('invalid argument', 9);
it('outside', function() {});
"#;
        check(
            "tests/component.spec.js",
            src,
            &[
                ("prepare", Role::Support),
                ("registered case", Role::Test),
                ("shadowed", Role::Support),
                ("invalid argument", Role::Support),
                ("it(label, handler)", Role::Support),
                ("outside", Role::Test),
            ],
        );
    }

    #[test]
    fn namespace_commonjs_and_non_runner_imports() {
        let src = r#"import * as suite from 'vitest';
import {test} from './ordinary.js';
const {test: check} = require('@jest/globals');
suite.test('namespace case', async () => {});
check('commonjs case', () => {});
test('unrelated import', () => {});
const nt = require('node:test');
nt.only('node default modifier', () => {});
"#;
        check(
            "test/check.js",
            src,
            &[
                ("namespace case", Role::Test),
                ("commonjs case", Role::Test),
                ("unrelated import", Role::Support),
                ("node default modifier", Role::Test),
            ],
        );
    }
    #[test]
    fn local_reexports_require_a_forwarded_registration() {
        use crate::domain::code::roles::classify_with;
        let source = "import { caseFn, fake, warm } from './bridge.js';
caseFn('forwarded', () => {});
fake('ignored', () => {});
warm('ignored fixed title', () => {});
";
        let mut reads = Vec::new();
        let result = classify_with("src/main.ts", source, &mut Default::default(), |p| {
            reads.push(p.to_owned());
            Ok(match p.to_str().unwrap() {
                "src/bridge.ts" => {
                    Some("export { run as caseFn, fake, warm } from './adapter.js';")
                }
                "src/adapter.ts" => Some(
                    "import { test as native } from 'node:test';
function register(variant, title, body) {return variant(title, body);}
export function run(title, body) {return register(native,title,body);}
export function warm(title, body) {return register(native,'fixed',body);}
export function fake(title, body) {return [native, title, body];}",
                ),
                _ => None,
            }
            .map(String::from))
        })
        .unwrap();
        for file in ["src/bridge.ts", "src/adapter.ts"] {
            assert_eq!(
                reads
                    .iter()
                    .filter(|p| p.as_path() == std::path::Path::new(file))
                    .count(),
                1
            );
        }
        for (marker, want) in [
            ("forwarded", true),
            ("ignored", false),
            ("ignored fixed title", false),
        ] {
            let at = source.find(marker).unwrap();
            assert_eq!(
                result
                    .regions
                    .iter()
                    .any(|r| r.start <= at && at < r.end && r.evidence.0 == Role::Test),
                want,
                "{marker}"
            );
        }
    }

    #[test]
    fn a_reexport_cycle_can_use_a_previously_resolved_sibling() {
        use crate::domain::code::roles::classify_with;
        for (names, tests) in [("first,second", 2), ("second,first", 1)] {
            let source = format!(
                "import {{{names}}} from './a.ts';\nfirst('A',()=>{{}});\nsecond('B',()=>{{}});"
            );
            let roles = classify_with("main.ts", &source, &mut Default::default(), |path| {
                Ok(match path.to_str().unwrap() {
                    "a.ts" => Some("export {first,second} from './b.ts';"),
                    "b.ts" => Some("export {test as first} from 'vitest'; export {first as second} from './a.ts';"),
                    _ => None,
                }.map(str::to_owned))
            }).unwrap();
            let cases: Vec<_> = roles
                .regions
                .iter()
                .filter(|r| r.evidence.0 == Role::Test)
                .collect();
            assert_eq!(cases.len(), tests, "{names}");
            assert!(source[cases[0].start..cases[0].end].contains("'A'"));
        }
    }

    #[test]
    fn named_imports_share_the_module_read_and_keep_separate_evidence() {
        use crate::domain::code::roles::classify_with;
        let source = concat!(
            "import { real, helper, alias, unused } from './suite.js';\n",
            "real('first', ()=>{});\n",
            "helper('ordinary', ()=>{});\n",
            "alias('last', ()=>{});\n",
        );
        let mut reads = Vec::new();
        let mut context = Default::default();
        let roles = classify_with("src/main.ts", source, &mut context, |path| {
            reads.push(path.to_owned());
            Ok((path == std::path::Path::new("src/suite.ts")).then(|| {
                concat!(
                    "import { test } from 'vitest';\n",
                    "export function real(title, fn) { test(title, fn); }\n",
                    "export function helper(title, fn) { elsewhere(title, fn); }\n",
                    "export { real as alias };\n",
                    "export { unused } from './unneeded.js';\n",
                )
                .to_owned()
            }))
        })
        .unwrap();
        assert_eq!(
            reads,
            [
                std::path::Path::new("src/suite.js"),
                std::path::Path::new("src/suite.ts")
            ]
        );
        let lines: Vec<_> = roles
            .regions
            .iter()
            .filter(|r| r.evidence.0 == Role::Test)
            .map(|r| &source[r.start..r.end])
            .collect();
        let sibling = classify_with("src/other.ts", source, &mut context, |_| {
            panic!("verified export routes should be reused in this snapshot")
        })
        .unwrap();
        assert_eq!(
            sibling
                .regions
                .iter()
                .map(|r| (r.start, r.end))
                .collect::<Vec<_>>(),
            roles
                .regions
                .iter()
                .map(|r| (r.start, r.end))
                .collect::<Vec<_>>()
        );
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("first") && lines[1].contains("last"));
    }

    #[test]
    fn reexported_parameterized_test_uses_the_same_call_syntax_as_a_direct_import() {
        use crate::domain::code::roles::classify_with;
        let source = "import {run} from './suite'; run.each([1])('case', n => check(n));";
        let roles = classify_with("main.ts", source, &mut Default::default(), |path| {
            Ok((path == std::path::Path::new("suite.ts"))
                .then(|| "export {test as run} from 'vitest';".to_owned()))
        })
        .unwrap();
        let [case] = roles.regions.as_slice() else {
            panic!("expected one case")
        };
        assert_eq!(case.evidence.0, Role::Test);
        assert_eq!(
            &source[case.start..case.end],
            "run.each([1])('case', n => check(n))"
        );
    }

    #[test]
    fn ordinary_literal_calls_skip_module_io() {
        use crate::domain::code::roles::classify_with;
        classify_with(
            "src/main.ts",
            "import { load } from './unused.js'; load('data');",
            &mut Default::default(),
            |_| panic!("no callback-shaped registration"),
        )
        .unwrap();
    }

    #[test]
    fn nested_arrow_scopes_keep_group_and_statement_boundaries() {
        use super::{ExpressionBodies, Syntax, declarations::registration_expression_end};
        use crate::domain::code::{Language, lexer, script};
        let text = "let f = x => y => call(z => z, 1)\nconst g = a => b =>\nfunction(){};";
        let tokens = lexer::tokens(text, Language::Js);
        let pairs = script::paired_for_score(&tokens);
        let syntax = Syntax {
            tokens: &tokens,
            pairs: &pairs,
            text,
        };
        let mut expressions = ExpressionBodies::default();
        let bounds: Vec<_> = (0..tokens.len())
            .filter(|&i| syntax.seq(i, &["=", ">"]))
            .map(|i| {
                syntax.w(registration_expression_end(
                    &mut expressions,
                    &syntax,
                    i + 2,
                ))
            })
            .collect();
        assert_eq!(bounds, ["const", "const", ",", "function", ";"]);
    }

    #[test]
    fn malformed_import_does_not_publish_its_valid_prefix() {
        let text = "import {test as bad, 123} from 'vitest';\nimport {test as good} from 'vitest';\nbad('skip',()=>{});\ngood('keep',()=>{});";
        check(
            "src/main.ts",
            text,
            &[
                ("bad('skip'", Role::Production),
                ("good('keep'", Role::Test),
            ],
        );
    }

    #[test]
    fn cyclic_exports_terminate_without_creating_a_test() {
        use crate::domain::code::roles::classify_with;
        let result = classify_with(
            "src/main.ts",
            "import { check } from './a.js'; check('title', () => {});",
            &mut Default::default(),
            |p| {
                Ok(match p.to_str().unwrap() {
                    "src/a.js" => Some("export { check } from './b.js';"),
                    "src/b.js" => Some("export { check } from './a.js';"),
                    _ => None,
                }
                .map(String::from))
            },
        )
        .unwrap();
        assert!(result.regions.is_empty());
    }
}
