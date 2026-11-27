use super::{Analysis, Token, before, call, colon_name, declare, word};
use crate::domain::code::record_unique;
use std::collections::HashMap;

fn method_name(t: &[Token<'_>], n: usize, declaration: bool) -> (std::ops::Range<usize>, usize) {
    let span = t[n].span();
    let Some(next) = t.get(n + 1).filter(|tok| tok.at == span.end) else {
        return (span, n + 1);
    };
    match next.text {
        "!" | "?" => (span.start..next.span().end, n + 2),
        "=" if declaration => (span.start..next.span().end, n + 2),
        _ => (span, n + 1),
    }
}

fn dotted_method(t: &[Token], receiver: usize) -> Option<usize> {
    let member = receiver + 2;
    (word(t, receiver + 1) == "." && t.get(member).is_some_and(|tok| tok.name())).then_some(member)
}

fn method_header<'a>(
    t: &[Token<'a>],
    i: usize,
    class: Option<&'a str>,
    top: bool,
) -> Option<(Option<&'a str>, std::ops::Range<usize>, usize)> {
    let first = colon_name(t, i + 1)?;
    let Some(n) = dotted_method(t, first) else {
        let (span, after) = method_name(t, first, true);
        return Some((None, span, after));
    };
    let receiver = match (first == i + 1, t[first].text) {
        (true, "self") => class,
        (true, name) if top && name.starts_with(char::is_uppercase) => Some(name),
        _ => None,
    };
    let (span, after) = method_name(t, n, true);
    Some((receiver, span, after))
}

#[derive(Default)]
struct Singletons<'a> {
    // A qualified declaration or assignment permanently invalidates this spelling.
    constants: HashMap<&'a str, bool>,
    methods: HashMap<(&'a str, &'a str), Option<usize>>,
    pending: Vec<(&'a str, std::ops::Range<usize>, Option<usize>)>,
}

impl<'a> Singletons<'a> {
    fn observe(&mut self, source: &str, t: &[Token<'a>], i: usize, owner: Option<usize>) {
        let tok = &t[i];
        if word(t, i + 1) == "=" {
            self.constants.insert(tok.text, false);
        }
        if before(t, i) == "def" {
            return;
        }
        let Some(member) = dotted_method(t, i) else {
            return;
        };
        let (span, j) = method_name(t, member, false);
        if word(t, j) == "=" && !source[span.end..t[j].at].contains('\n') {
            return;
        }
        self.pending.push((tok.text, span, owner));
    }

    fn bind(self, source: &str, out: &mut Analysis) {
        for (class, span, owner) in self.pending {
            if self.constants.get(class) == Some(&true)
                && let Some(Some(id)) = self.methods.get(&(class, &source[span.clone()]))
            {
                call(out, span, owner, *id);
            }
        }
    }
}

#[derive(Default)]
struct Scopes<'a> {
    owner: Option<usize>,
    class: Option<&'a str>,
    frames: Vec<(Option<&'a str>, Option<usize>)>,
}
impl<'a> Scopes<'a> {
    fn open(&mut self) {
        self.frames.push((self.class, self.owner));
    }
    fn close(&mut self) {
        if let Some(frame) = self.frames.pop() {
            (self.class, self.owner) = frame;
        }
    }
    fn open_class(
        &mut self,
        t: &[Token<'a>],
        i: usize,
        singletons: &mut Singletons<'a>,
        out: &mut Analysis,
    ) {
        let top = self.frames.is_empty();
        self.open();
        self.owner = None;
        self.class = None;
        let Some(n) = class_name(t, i) else {
            return;
        };
        let class = t[n].text;
        let kind = match t[i].text {
            "class" => "class_definition",
            _ => "module_declaration",
        };
        declare(out, t[n].span(), kind);
        if top && n == i + 1 {
            singletons.constants.entry(class).or_insert(true);
            self.class = Some(class);
        } else {
            singletons.constants.insert(class, false);
        }
    }
    fn open_method(
        &mut self,
        input: Source<'a, '_>,
        i: usize,
        singletons: &mut Singletons<'a>,
        out: &mut Analysis,
    ) {
        let Source {
            text: source,
            tokens: t,
        } = input;
        let Some((receiver, span, after)) = method_header(t, i, self.class, self.frames.is_empty())
        else {
            return;
        };
        let name = &source[span.clone()];
        let id = declare(out, span, "method_definition");
        if let Some(class) = receiver {
            record_unique(&mut singletons.methods, (class, name), id);
        }
        // An endless method has no closing end to reclaim its ownership.
        if word(t, super::after_parameters(t, after)) != "=" {
            self.open();
            self.owner = Some(id);
        }
    }
}

fn class_name(t: &[Token], keyword: usize) -> Option<usize> {
    colon_name(t, keyword + 1).filter(|&name| t[name].text.starts_with(char::is_uppercase))
}

fn opens_control_block(source: &str, t: &[Token], i: usize) -> bool {
    i == 0
        || matches!(before(t, i), ";" | "=" | "(" | "=>")
        || source[t[i - 1].span().end..t[i].at].contains('\n')
}

#[derive(Clone, Copy)]
struct Source<'src, 'tok> {
    text: &'src str,
    tokens: &'tok [Token<'src>],
}

enum ScopeWord {
    End,
    Class,
    Method,
    Open,
    Conditional,
    Other,
}

impl ScopeWord {
    fn read(word: &str) -> Self {
        match word {
            "end" => Self::End,
            "class" | "module" => Self::Class,
            "def" => Self::Method,
            "do" | "case" | "begin" | "for" => Self::Open,
            "if" | "unless" | "while" | "until" => Self::Conditional,
            _ => Self::Other,
        }
    }
}

pub(super) fn analyze(source: &str, t: &[Token<'_>]) -> Analysis {
    let mut out = Analysis::default();
    let mut scope = Scopes::default();
    let mut singletons = Singletons::default();
    let input = Source {
        text: source,
        tokens: t,
    };
    for i in (0..t.len()).filter(|&i| !matches!(before(t, i), "." | ":" | "@" | "$")) {
        scope.observe(input, i, &mut singletons, &mut out);
    }
    singletons.bind(source, &mut out);
    out
}

impl<'a> Scopes<'a> {
    fn observe(
        &mut self,
        input: Source<'a, '_>,
        i: usize,
        singletons: &mut Singletons<'a>,
        out: &mut Analysis,
    ) {
        let token = &input.tokens[i];
        match ScopeWord::read(token.text) {
            ScopeWord::End => self.close(),
            ScopeWord::Class => self.open_class(input.tokens, i, singletons, out),
            ScopeWord::Method => self.open_method(input, i, singletons, out),
            ScopeWord::Open => self.open(),
            ScopeWord::Conditional => self.open_conditional(input, i),
            ScopeWord::Other => singletons.observe_constant(input, i, self.owner),
        }
    }

    fn open_conditional(&mut self, input: Source, i: usize) {
        if opens_control_block(input.text, input.tokens, i) {
            self.open();
        }
    }
}

impl<'a> Singletons<'a> {
    fn observe_constant(&mut self, input: Source<'a, '_>, i: usize, owner: Option<usize>) {
        if input.tokens[i].text.starts_with(char::is_uppercase) {
            self.observe(input.text, input.tokens, i, owner);
        }
    }
}
