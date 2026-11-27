use super::{Analysis, Token, before, call, colon_name, declare, word};
use crate::domain::code::record_unique;
use std::collections::HashMap;

enum Body<'a> {
    Package(&'a str),
    Sub(usize),
}

fn qualified<'a>(written: &'a str, package: &'a str, name: &'a str) -> (&'a str, &'a str) {
    written.rsplit_once("::").unwrap_or((package, name))
}

/// A lexical sub shadows package lookup even when its target is ambiguous.
#[derive(Default)]
struct Subs<'a> {
    lexical: HashMap<&'a str, Option<(usize, usize)>>,
    packages: HashMap<(&'a str, &'a str), Option<usize>>,
}

impl<'a> Subs<'a> {
    fn hide(&mut self, name: &'a str, target: Option<(usize, usize)>) {
        self.lexical
            .entry(name)
            .and_modify(|slot| *slot = None)
            .or_insert(target);
    }

    fn target(&self, at: usize, pkg: &str, name: &str, qualified: bool) -> Option<usize> {
        match (qualified, self.lexical.get(name)) {
            (false, Some(slot)) => slot.and_then(|(n, id)| (at > n).then_some(id)),
            _ => self.packages.get(&(pkg, name)).copied().flatten(),
        }
    }
}

struct Scopes<'a> {
    owner: Option<usize>,
    package: &'a str,
    blocks: Vec<(Option<usize>, &'a str)>,
    bodies: HashMap<usize, Body<'a>>,
}

impl<'a> Scopes<'a> {
    fn new() -> Self {
        Self {
            owner: None,
            package: "main",
            blocks: Vec::new(),
            bodies: HashMap::new(),
        }
    }
    fn brace(&mut self, word: &str, at: usize) -> bool {
        match word {
            "{" => {
                self.blocks.push((self.owner, self.package));
                match self.bodies.remove(&at) {
                    Some(Body::Sub(id)) => self.owner = Some(id),
                    Some(Body::Package(name)) => self.package = name,
                    None => {}
                }
            }
            "}" => self.close(),
            _ => return false,
        }
        true
    }
    fn close(&mut self) {
        if let Some(frame) = self.blocks.pop() {
            (self.owner, self.package) = frame;
        }
    }

    fn declare_package(&mut self, name: WrittenName<'a>, out: &mut Analysis) {
        declare(out, name.span, "package_declaration");
        if name.next == "{" {
            self.bodies.insert(name.last + 1, Body::Package(name.full));
        } else {
            self.package = name.full;
        }
    }

    fn declare_sub(
        &mut self,
        tokens: &[Token<'a>],
        (keyword, name): (usize, WrittenName<'a>),
        subs: &mut Subs<'a>,
        out: &mut Analysis,
    ) {
        let kind = if name.next == ";" {
            "function_declaration"
        } else {
            "function_definition"
        };
        let id = declare(out, name.bare.span(), kind);
        let body = super::after_parameters(tokens, name.last + 1);
        if word(tokens, body) != "{" {
            return;
        }
        self.bodies.insert(body, Body::Sub(id));
        if matches!(before(tokens, keyword), "my" | "state") {
            let target =
                (self.blocks.is_empty() && name.last == keyword + 1).then_some((name.last, id));
            subs.hide(name.bare.text, target);
        } else {
            let qualified = qualified(name.full, self.package, name.bare.text);
            record_unique(&mut subs.packages, qualified, id);
        }
    }
}

struct WrittenName<'a> {
    last: usize,
    span: std::ops::Range<usize>,
    full: &'a str,
    bare: Token<'a>,
    next: &'a str,
}

impl<'a> WrittenName<'a> {
    fn read(source: &'a str, tokens: &[Token<'a>], start: usize) -> Option<Self> {
        let last = colon_name(tokens, start)?;
        let bare = tokens[last];
        let span = tokens[start].at..bare.span().end;
        Some(Self {
            last,
            full: &source[span.clone()],
            span,
            bare,
            next: word(tokens, last + 1),
        })
    }
}

enum Event<'a> {
    Package(WrittenName<'a>),
    Sub(WrittenName<'a>),
    Call(WrittenName<'a>),
}

impl<'a> Event<'a> {
    fn read(source: &'a str, tokens: &[Token<'a>], i: usize) -> Option<Self> {
        let prev = before(tokens, i);
        let token = tokens[i];
        if !token.name() || matches!(prev, ">" | ":" | "$" | "@" | "%") {
            return None;
        }
        if prev != "&" {
            match token.text {
                "package" => return WrittenName::read(source, tokens, i + 1).map(Self::Package),
                "sub" => return WrittenName::read(source, tokens, i + 1).map(Self::Sub),
                _ => {}
            }
        }
        Self::read_call(source, tokens, i, prev)
    }

    fn read_call(source: &'a str, tokens: &[Token<'a>], i: usize, previous: &str) -> Option<Self> {
        if matches!(previous, "sub" | "package") {
            return None;
        }
        WrittenName::read(source, tokens, i)
            .filter(|name| name.next == "(")
            .map(Self::Call)
    }
}

pub(super) fn analyze<'a>(source: &'a str, t: &[Token<'a>]) -> Analysis {
    let mut file = FileBindings::default();
    for i in 0..t.len() {
        file.observe(source, t, i);
    }
    file.finish(t)
}

struct FileBindings<'a> {
    out: Analysis,
    scope: Scopes<'a>,
    subs: Subs<'a>,
    pending: Vec<(usize, Option<usize>, &'a str, bool)>,
}

impl Default for FileBindings<'_> {
    fn default() -> Self {
        Self {
            out: Analysis::default(),
            scope: Scopes::new(),
            subs: Subs::default(),
            pending: Vec::new(),
        }
    }
}

impl<'a> FileBindings<'a> {
    fn observe(&mut self, source: &'a str, t: &[Token<'a>], i: usize) {
        if self.scope.brace(t[i].text, i) {
            return;
        }
        match Event::read(source, t, i) {
            Some(Event::Package(name)) => self.scope.declare_package(name, &mut self.out),
            Some(Event::Sub(name)) => {
                self.scope
                    .declare_sub(t, (i, name), &mut self.subs, &mut self.out)
            }
            Some(Event::Call(name)) => {
                let (pkg, _) = qualified(name.full, self.scope.package, name.bare.text);
                self.pending
                    .push((name.last, self.scope.owner, pkg, i != name.last));
            }
            None => {}
        }
    }

    fn finish(mut self, t: &[Token<'a>]) -> Analysis {
        let targets = self
            .pending
            .into_iter()
            .filter_map(|(i, owner, pkg, qualified)| {
                Some((i, owner, self.subs.target(i, pkg, t[i].text, qualified)?))
            });
        for (i, owner, id) in targets {
            call(&mut self.out, t[i].span(), owner, id);
        }
        self.out
    }
}
