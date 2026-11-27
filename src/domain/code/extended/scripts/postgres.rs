use super::{Analysis, Language, Token, declare, tokens, word};
use std::ops::Range;

fn name(tok: Token<'_>) -> Option<Range<usize>> {
    if tok.name() {
        Some(tok.span())
    } else {
        let interior = tok.text.strip_prefix('"')?.strip_suffix('"')?;
        (!interior.is_empty()).then_some(tok.at + 1..tok.at + 1 + interior.len())
    }
}
fn key(tok: Token<'_>) -> String {
    if tok.text.starts_with('"') {
        tok.text.trim_matches('"').replace("\"\"", "\"")
    } else {
        tok.text.to_ascii_lowercase()
    }
}
struct Block {
    label: Option<(String, usize)>,
    is_loop: bool,
}
impl Block {
    fn jump(blocks: &[Self], wanted: &str, exit: bool) -> Option<usize> {
        let (id, block) = blocks
            .iter()
            .rev()
            .find_map(|block| Some((block.target(wanted)?, block)))?;
        (exit || block.is_loop).then_some(id)
    }

    fn target(&self, name: &str) -> Option<usize> {
        let (key, target) = self.label.as_ref()?;
        (key == name).then_some(*target)
    }
}

fn label_header(t: &[Token], i: usize) -> Option<(String, Range<usize>)> {
    let [left, candidate, right, close, body, ..] = t.get(i..)? else {
        return None;
    };
    if (left.text, right.text, close.text) != ("<", ">", ">")
        || !["declare", "begin", "loop", "while", "for", "foreach"]
            .iter()
            .any(|w| body.text.eq_ignore_ascii_case(w))
    {
        return None;
    }
    Some((key(*candidate), name(*candidate)?))
}

fn label_use(
    token: Option<&Token>,
    out: &mut Analysis,
    owner: Option<usize>,
    find: impl FnOnce(&str) -> Option<usize>,
) {
    let Some(&token) = token else { return };
    let Some(span) = name(token) else { return };
    if let Some(target) = find(&key(token)) {
        out.direct_edge(target, span, owner, false);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockWord {
    Begin,
    Loop,
    Branch,
    Exit,
    Continue,
    End,
    Label,
    Other,
}
impl BlockWord {
    fn read(word: &str) -> Self {
        const WORDS: &[(&str, BlockWord)] = &[
            ("begin", BlockWord::Begin),
            ("loop", BlockWord::Loop),
            ("if", BlockWord::Branch),
            ("case", BlockWord::Branch),
            ("exit", BlockWord::Exit),
            ("continue", BlockWord::Continue),
            ("end", BlockWord::End),
            ("<", BlockWord::Label),
        ];
        WORDS
            .iter()
            .find_map(|&(key, kind)| word.eq_ignore_ascii_case(key).then_some(kind))
            .unwrap_or(Self::Other)
    }
}

#[derive(Default)]
struct Blocks {
    stack: Vec<Block>,
    pending: Option<(String, usize)>,
}

impl Blocks {
    fn open(&mut self, is_loop: bool) {
        self.stack.push(Block {
            label: self.pending.take(),
            is_loop,
        });
    }

    fn header(&mut self, tokens: &[Token], next: usize, out: &mut Analysis) -> usize {
        let Some((key, span)) = label_header(tokens, next) else {
            return next;
        };
        self.pending = Some((key, declare(out, span, "label")));
        next + 4
    }

    fn close(
        &mut self,
        tokens: &[Token],
        next: usize,
        owner: Option<usize>,
        out: &mut Analysis,
    ) -> usize {
        let block = self.stack.pop();
        let label = next
            + usize::from(matches!(
                BlockWord::read(word(tokens, next)),
                BlockWord::Loop | BlockWord::Branch
            ));
        if let Some(block) = block.filter(|block| block.label.is_some()) {
            label_use(tokens.get(label), out, owner, |wanted| block.target(wanted));
        }
        label
    }

    fn observe(
        &mut self,
        tokens: &[Token],
        (at, word): (usize, BlockWord),
        owner: Option<usize>,
        out: &mut Analysis,
    ) -> usize {
        let next = at + 1;
        match word {
            BlockWord::Label => return self.header(tokens, next, out),
            BlockWord::Begin | BlockWord::Loop => self.open(word == BlockWord::Loop),
            BlockWord::Branch => self.stack.push(Block {
                label: None,
                is_loop: false,
            }),
            BlockWord::Exit | BlockWord::Continue => {
                label_use(tokens.get(next), out, owner, |wanted| {
                    Block::jump(&self.stack, wanted, word == BlockWord::Exit)
                });
            }
            BlockWord::End => return self.close(tokens, next, owner, out),
            BlockWord::Other => {}
        }
        next
    }
}

fn labels(tokens: &[Token], out: &mut Analysis, owner: Option<usize>) {
    let mut blocks = Blocks::default();
    let mut at = 0;
    while let Some(token) = tokens.get(at) {
        at = blocks.observe(tokens, (at, BlockWord::read(token.text)), owner, out);
    }
}

struct Routine {
    name: Range<usize>,
    raw: Range<usize>,
    function: bool,
    procedural: bool,
    body: Option<Range<usize>>,
}

#[derive(Default)]
struct RoutineOptions<'a> {
    plpgsql: bool,
    body: Option<Token<'a>>,
}

impl<'a> RoutineOptions<'a> {
    fn observe(&mut self, tokens: &[Token<'a>], at: usize, keyword: &str) {
        if keyword.eq_ignore_ascii_case("as") {
            self.body = tokens.get(at + 1).copied();
            return;
        }
        if keyword.eq_ignore_ascii_case("language") && admits_language(word(tokens, at - 1)) {
            let spelling = word(tokens, at + 1);
            self.plpgsql = spelling.eq_ignore_ascii_case("plpgsql")
                || matches!(spelling, "\"plpgsql\"" | "'plpgsql'");
        }
    }

    fn body_span(&self) -> Option<Range<usize>> {
        let token = self.body.filter(|_| self.plpgsql)?;
        let content = crate::domain::code::lexer::dollar_content(token.text)?;
        Some(token.at + content.start..token.at + content.end)
    }
}

fn admits_language(previous: &str) -> bool {
    !previous.eq_ignore_ascii_case("set") && !previous.eq_ignore_ascii_case("returns")
}

fn routine_body(t: &[Token<'_>], start: usize) -> (usize, bool, Option<Range<usize>>) {
    let mut depth = 0usize;
    let mut options = RoutineOptions::default();
    let mut end = start;
    while end < t.len() && t[end].text != ";" {
        match t[end].text {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            keyword if depth == 0 => options.observe(t, end, keyword),
            _ => {}
        }
        end += 1;
    }
    (end, options.plpgsql, options.body_span())
}

fn after_create<'a>(at: usize, word: impl Fn(usize) -> &'a str) -> usize {
    if word(at).eq_ignore_ascii_case("or") && word(at + 1).eq_ignore_ascii_case("replace") {
        at + 2
    } else {
        at
    }
}

impl Routine {
    fn after_create(t: &[Token<'_>], i: usize) -> Option<(usize, Self)> {
        let mut j = after_create(i, |j| word(t, j));
        let function = word(t, j).eq_ignore_ascii_case("function");
        if !function && !word(t, j).eq_ignore_ascii_case("procedure") {
            return None;
        }
        j += 1;
        let mut span = name(*t.get(j)?)?;
        while word(t, j + 1) == "."
            && let Some(part) = t.get(j + 2).and_then(|n| name(*n))
        {
            j += 2;
            span = part;
        }
        if word(t, j + 1) != "(" {
            return None;
        }
        let raw_span = t[j].span();
        let (end, plpgsql, body) = routine_body(t, j + 1);
        Some((
            end,
            Self {
                name: span,
                raw: raw_span,
                function,
                procedural: plpgsql,
                body,
            },
        ))
    }
}

fn visit_routines(t: &[Token<'_>], mut visit: impl FnMut(Routine)) {
    let mut i = 0;
    while let Some(token) = t.get(i) {
        i += 1;
        if token.text.eq_ignore_ascii_case("create")
            && let Some((end, routine)) = Routine::after_create(t, i)
        {
            i = end;
            visit(routine);
        }
    }
}

pub(in crate::domain::code) fn body_comments(
    source: &str,
    t: &[Token<'_>],
    mut comment: impl FnMut(usize),
) -> bool {
    let mut found = false;
    visit_routines(t, |r| {
        found |= r.procedural;
        if let Some(body) = r.body {
            super::tokens_with_comments(&source[body.clone()], Language::PlPgSql, |at| {
                comment(body.start + at)
            });
        }
    });
    found
}

impl Routine {
    fn bind_outer(&self, out: &mut Analysis, outer_count: usize, next: &mut usize) -> usize {
        while *next < outer_count && out.declarations[*next].span.start < self.raw.start {
            *next += 1;
        }
        let kind = if self.function {
            "plpgsql_function"
        } else {
            "plpgsql_procedure"
        };
        if *next < outer_count
            && (out.declarations[*next].span == self.raw
                || out.declarations[*next].span == self.name)
        {
            let id = *next;
            *next += 1;
            out.declarations[id].span = self.name.clone();
            out.declarations[id].kind = kind;
            id
        } else {
            declare(out, self.name.clone(), kind)
        }
    }

    fn body_labels(&self, source: &str, out: &mut Analysis, owner: usize) {
        if let Some(body) = &self.body {
            let mut inner = tokens(&source[body.clone()], Language::PlPgSql);
            for token in &mut inner {
                token.at += body.start;
            }
            labels(&inner, out, Some(owner));
        }
    }
}

// The outer SQL grammar also recognizes non-PostgreSQL identifier spellings
// such as backticks. Keep its source spans even when a procedural body is present.
fn object_name_start<'a>(at: usize, word: impl Fn(usize) -> &'a str) -> Option<usize> {
    let mut at = after_create(at, &word);
    while ["temporary", "temp", "unlogged", "materialized"]
        .iter()
        .any(|kind| word(at).eq_ignore_ascii_case(kind))
    {
        at += 1;
    }
    if !["table", "view", "procedure", "function", "sequence"]
        .iter()
        .any(|kind| word(at).eq_ignore_ascii_case(kind))
    {
        return None;
    }
    let start = at + 1;
    let extension = ["if", "not", "exists"]
        .iter()
        .enumerate()
        .all(|(offset, part)| word(start + offset).eq_ignore_ascii_case(part));
    Some(start + 3 * usize::from(extension))
}

fn object_name<'a>(
    t: &'a [crate::domain::code::Token],
    at: usize,
) -> Option<&'a crate::domain::code::Token<'a>> {
    use crate::domain::code::Kind;
    let word = |i: usize| t.get(i).map_or("", |tok| tok.text);
    let mut i = object_name_start(at, word)?;
    while word(i + 1) == "." {
        i += 2;
    }
    let name = t.get(i)?;
    (name.kind == Kind::Word || name.kind == Kind::String && name.text.starts_with(['"', '`']))
        .then_some(name)
}

fn procedural_language(keyword: &str, name: Option<&crate::domain::code::Token>) -> bool {
    keyword.eq_ignore_ascii_case("language")
        && name.is_some_and(|token| {
            token
                .text
                .trim_matches(['\'', '"'])
                .eq_ignore_ascii_case("plpgsql")
        })
}

fn outer_names(outer: &[crate::domain::code::Token], out: &mut Analysis) -> bool {
    let mut mixed = false;
    for (i, token) in outer.iter().enumerate() {
        let next = i + 1;
        if token.text.eq_ignore_ascii_case("create") {
            out.declarations
                .extend(
                    object_name(outer, next).map(|name| crate::domain::code::Declaration {
                        span: name.span.clone(),
                        kind: "object_reference",
                    }),
                );
            continue;
        }
        mixed |= !mixed && procedural_language(token.text, outer.get(next));
    }
    mixed
}

pub(in crate::domain::code) fn sql(source: &str, outer: &[crate::domain::code::Token]) -> Analysis {
    let mut out = Analysis::default();
    if !outer_names(outer, &mut out) {
        return out;
    }
    // Declarations and procedural tokens are both in source order. Reject
    // outer names hidden inside a nested comment or dollar-quoted literal.
    let t = tokens(source, Language::PlPgSql);
    let mut token = t.iter().peekable();
    out.declarations.retain(|d| {
        while token.next_if(|t| t.at < d.span.start).is_some() {}
        token.peek().is_some_and(|t| t.at == d.span.start)
    });
    // Upgrade the existing declaration in place so forward IDs keep their
    // source order. Body labels and new routine names follow all outer names.
    let outer_count = out.declarations.len();
    let mut next = 0;
    visit_routines(&t, |r| {
        if !r.procedural {
            return;
        }
        let owner = r.bind_outer(&mut out, outer_count, &mut next);
        r.body_labels(source, &mut out, owner);
    });
    out
}

pub(super) fn analyze(source: &str, t: &[Token<'_>]) -> Analysis {
    let mut out = Analysis::default();
    visit_routines(t, |r| {
        let kind = if r.function {
            "function_definition"
        } else {
            "procedure_declaration"
        };
        let owner = declare(&mut out, r.name.clone(), kind);
        r.body_labels(source, &mut out, owner);
    });
    labels(t, &mut out, None);
    out
}
