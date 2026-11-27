use super::super::{Facts, UseMode, name_byte, simple, span, target};
use crate::domain::code::{Analysis, make_reference::Delimiter};

fn variable_name(rest: &str) -> Option<(Delimiter, &str, &str)> {
    let (delimiter, inside) = Delimiter::open(rest)?;
    let stop = inside
        .bytes()
        .position(|b| !name_byte(b))
        .unwrap_or(inside.len());
    let (name, tail) = inside.split_at(stop);
    Some((delimiter, name, tail))
}

fn variable_reads(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        let (_, rest) = text.split_once('$')?;
        text = rest;
        Some(variable_read(&mut text))
    })
    .flatten()
}

fn variable_read<'a>(text: &mut &'a str) -> Option<&'a str> {
    if let Some(next) = text.strip_prefix('$') {
        *text = next;
        return None;
    }
    let (delimiter, name, tail) = variable_name(text)?;
    *text = tail;
    if name.is_empty() {
        return None;
    }
    *text = delimiter.close(tail)?;
    Some(name)
}

fn make_vars(source: &str, line: &str, owner: Option<usize>, f: &mut Facts) {
    for name in variable_reads(line) {
        f.usage(
            format!("v:{name}"),
            span(source, name),
            owner,
            UseMode::Reference,
        );
    }
}

enum MakeLine<'a> {
    Continued(&'a str),
    Ignore,
    Define,
    Recipe(&'a str),
    Assignment(&'a str, &'a str),
    Rule(&'a str),
    Comment(usize),
}

struct MakeSyntax {
    continuing: bool,
    define_depth: usize,
    recipe_prefix: char,
}

impl MakeSyntax {
    fn line<'a>(&mut self, source: &str, line: &'a str) -> MakeLine<'a> {
        let stripped = line.strip_suffix('\\');
        let raw = stripped.unwrap_or(line);
        if std::mem::replace(&mut self.continuing, stripped.is_some()) {
            MakeLine::Continued(raw)
        } else {
            self.classify(source, raw)
        }
    }

    fn classify<'a>(&mut self, source: &str, raw: &'a str) -> MakeLine<'a> {
        if self.define_depth > 0 {
            self.update_define_depth(raw);
            return MakeLine::Ignore;
        }
        if let Some(recipe) = raw.strip_prefix(self.recipe_prefix) {
            return MakeLine::Recipe(recipe);
        }
        let trimmed = raw.trim_start();
        if trimmed.starts_with('#') {
            return MakeLine::Comment(span(source, trimmed).start);
        }
        let line = trimmed.split('#').next().unwrap_or("").trim_end();
        if line.is_empty() {
            return MakeLine::Ignore;
        }
        if line.split_ascii_whitespace().next() == Some("define") {
            self.define_depth = 1;
            return MakeLine::Define;
        }
        self.assignment_or_rule(line)
    }

    fn update_define_depth(&mut self, line: &str) {
        match line.split_ascii_whitespace().next() {
            Some("define") => self.define_depth += 1,
            Some("endef") => self.define_depth -= 1,
            _ => {}
        }
    }

    fn assignment_or_rule<'a>(&mut self, line: &'a str) -> MakeLine<'a> {
        let Some((lhs, rhs)) = line.split_once('=') else {
            return MakeLine::Rule(line);
        };
        let name = lhs
            .trim_end()
            .trim_end_matches([':', '?', '+', '!'])
            .trim_end();
        if !simple(name) {
            return MakeLine::Rule(line);
        }
        if name == ".RECIPEPREFIX" {
            self.recipe_prefix = rhs.trim_start().chars().next().unwrap_or('\t');
        }
        MakeLine::Assignment(name, rhs)
    }
}

fn make_lines(source: &str, mut visit: impl FnMut(MakeLine<'_>, bool)) {
    let mut syntax = MakeSyntax {
        define_depth: 0,
        recipe_prefix: '\t',
        continuing: false,
    };
    for line in source.lines() {
        let kind = syntax.line(source, line);
        visit(kind, syntax.continuing);
    }
}

pub(in crate::domain::code) fn make_comments(source: &str, mut comment: impl FnMut(usize)) {
    make_lines(source, |line, _| {
        if let MakeLine::Comment(at) = line {
            comment(at);
        }
    });
}

enum Continuation {
    Ignore,
    Prerequisites(Option<usize>),
    Variables(Option<usize>),
}
impl Continuation {
    fn read(&self, source: &str, raw: &str, facts: &mut Facts) {
        match *self {
            Continuation::Prerequisites(id) => {
                make_prerequisites(source, raw.split('#').next().unwrap_or(""), id, facts);
            }
            Continuation::Variables(id) => make_vars(source, raw, id, facts),
            Continuation::Ignore => {}
        }
    }
}

pub(in crate::domain::code) fn make(source: &str) -> Analysis {
    let mut f = Facts::default();
    let mut owner = None;
    let mut continuation = Continuation::Ignore;
    make_lines(source, |line, more| {
        continuation = match line {
            MakeLine::Continued(raw) => {
                continuation.read(source, raw, &mut f);
                return;
            }
            MakeLine::Ignore | MakeLine::Comment(_) => Continuation::Ignore,
            MakeLine::Define => {
                owner = None;
                Continuation::Ignore
            }
            MakeLine::Recipe(recipe) => {
                make_recipe(source, (recipe, more), owner, &mut f);
                Continuation::Variables(owner)
            }
            MakeLine::Assignment(name, rhs) => {
                let id = f.declare(
                    format!("v:{name}"),
                    span(source, name),
                    "variable_declarator",
                );
                owner = None;
                let next = Continuation::Variables(Some(id));
                next.read(source, rhs, &mut f);
                next
            }
            MakeLine::Rule(line) => {
                let Some((id, rhs)) = make_targets(source, line, &mut f) else {
                    owner = None;
                    continuation = Continuation::Ignore;
                    return;
                };
                owner = id;
                let next = Continuation::Prerequisites(owner);
                next.read(source, rhs, &mut f);
                next
            }
        };
    });
    f.finish()
}

fn make_recipe(
    source: &str,
    (recipe, continued): (&str, bool),
    owner: Option<usize>,
    f: &mut Facts,
) {
    let recipe = recipe
        .trim_start()
        .trim_start_matches(['@', '-', '+'])
        .trim_start();
    make_vars(source, recipe, owner, f);
    if continued {
        return;
    }
    let mut words = recipe.split_ascii_whitespace();
    if !matches!(words.next(), Some("$(MAKE)" | "${MAKE}")) {
        return;
    }
    if words.clone().all(|s| target(s) && !s.starts_with('-')) {
        for arg in words {
            f.usage(format!("t:{arg}"), span(source, arg), owner, UseMode::Call);
        }
    }
}

// A valid multi-target rule contributes prerequisites without assigning a
// single owner. An invalid rule contributes neither.
fn make_targets<'a>(
    source: &str,
    line: &'a str,
    f: &mut Facts,
) -> Option<(Option<usize>, &'a str)> {
    let (lhs, rhs) = line.split_once(':')?;
    let rhs = rhs.trim_start_matches(':').split(';').next().unwrap_or("");
    let mut targets = lhs.split_ascii_whitespace();
    let first = targets.next()?;
    if rhs.contains(['=', ':']) {
        return None;
    }
    if !target(first) || !targets.clone().all(target) {
        return None;
    }
    let single = targets.clone().next().is_none();
    let mut declare_target = |name: &str| {
        (!special_target(name))
            .then(|| f.declare(format!("t:{name}"), span(source, name), "build_target"))
    };
    let first_id = declare_target(first);
    for name in targets {
        declare_target(name);
    }
    let owner = first_id.filter(|_| single);
    Some((owner, rhs))
}

fn special_target(name: &str) -> bool {
    name.strip_prefix('.')
        .is_some_and(|name| name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
}

fn make_prerequisites(source: &str, rhs: &str, owner: Option<usize>, f: &mut Facts) {
    for dep in rhs.split_ascii_whitespace().filter(|s| target(s)) {
        f.usage(
            format!("t:{dep}"),
            span(source, dep),
            owner,
            UseMode::Reference,
        );
    }
    make_vars(source, rhs, owner, f);
}
