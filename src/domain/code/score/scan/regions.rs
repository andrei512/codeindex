//! Locate the source ranges that own function-level measurements.
use super::{Analysis, Language, Token, at, ind, lower_keyword};
use crate::domain::code::{Kind, script};
use Language::*;
mod declared;
use crate::domain::code::score::PairedSyntax;

fn declaration(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition"
            | "function_item"
            | "function_declaration"
            | "constructor_declaration"
            | "method_definition"
            | "method_declaration"
            | "function_expression"
            | "generator_function_declaration"
            | "procedure_declaration"
            | "plpgsql_function"
            | "plpgsql_procedure"
            | "function_name"
            | "build_target"
            | "build_stage"
            | "process_type"
            | "batch_label"
            | "label"
            | "block_label"
            | "message_name"
            | "macro_definition"
    )
}
fn generic_parameters(lang: Language) -> bool {
    matches!(
        lang,
        Rust | Cpp | Csharp | Java | Kotlin | Swift | Js | Dart | Hack | ObjectiveC
    )
}
impl PairedSyntax<'_, '_> {
    fn parameters(self, lang: Language, contents: std::ops::Range<usize>) -> usize {
        self.count_parameters(lang, contents, false)
    }

    fn count_parameters(
        self,
        lang: Language,
        contents: std::ops::Range<usize>,
        omit_wildcards: bool,
    ) -> usize {
        let mut i = contents.start;
        let end = contents.end;
        let mut list = Parameters::default();
        let type_arguments = generic_parameters(lang);
        let mut omitted = 0;
        while i < end {
            omitted += usize::from(
                omit_wildcards
                    && !list.occupied
                    && at(self.tokens, i) == "_"
                    && at(self.tokens, i + 1) == ":",
            );
            i = list.observe(self, i..end, type_arguments) + 1;
        }
        list.count + usize::from(list.occupied) - omitted
    }
}
#[derive(Default)]
struct Parameters {
    count: usize,
    occupied: bool,
    default_value: bool,
}

impl Parameters {
    fn observe(
        &mut self,
        syntax: PairedSyntax,
        bounds: std::ops::Range<usize>,
        type_arguments: bool,
    ) -> usize {
        let (i, end) = (bounds.start, bounds.end);
        match syntax.tokens[i].text {
            "," => {
                self.count += usize::from(self.occupied);
                self.occupied = false;
                self.default_value = false;
            }
            "(" | "[" | "{" => {
                self.occupied = true;
                return syntax.pairs[i].min(end);
            }
            "<" => {
                self.occupied = true;
                return self.angle_end(syntax, bounds, type_arguments);
            }
            "=" => self.default_value = true,
            "/" | "*" => {}
            _ => self.occupied = true,
        }
        i
    }

    fn angle_end(
        &self,
        syntax: PairedSyntax,
        bounds: std::ops::Range<usize>,
        type_arguments: bool,
    ) -> usize {
        let i = bounds.start;
        if type_arguments && !self.default_value {
            return script::angle_end(&syntax.tokens[..bounds.end], i).unwrap_or(i);
        }
        i
    }
}

pub(in crate::domain::code::score) struct Region<'a> {
    pub(in crate::domain::code::score) name: &'a str,
    pub(in crate::domain::code::score) start: usize,
    pub(super) body: usize,
    pub(in crate::domain::code::score) end: usize,
    pub(super) args: usize,
}
// Decorator lines immediately preceding a Python method can override binding.
fn static_method(t: &[Token], name: usize) -> bool {
    let before = &t[..name];
    let mut lines = before.chunk_by(|a, b| a.line == b.line).rev();
    if before
        .last()
        .is_some_and(|token| token.line == t[name].line)
    {
        lines.next();
    }
    lines
        .take_while(|line| line[0].text == "@")
        .any(|line| match line {
            [_, name] => name.text == "staticmethod",
            [_, module, dot, name] => {
                module.text == "builtins" && dot.text == "." && name.text == "staticmethod"
            }
            _ => false,
        })
}
fn cmake_regions<'a>(syntax: PairedSyntax<'_, 'a>) -> Vec<Region<'a>> {
    let t = syntax.tokens;
    let mut regions = Vec::new();
    let mut stack = Vec::new();
    let mut i = 0;
    while i + 1 < t.len() {
        let Some(end) = syntax.close_paren(i + 1) else {
            i += 1;
            continue;
        };
        cmake_region(t, i..end, &mut stack, &mut regions);
        i = end + 1;
    }
    regions
}
fn cmake_region<'a>(
    t: &[Token<'a>],
    call: std::ops::Range<usize>,
    stack: &mut Vec<usize>,
    regions: &mut Vec<Region<'a>>,
) {
    let (i, end) = (call.start, call.end);
    let name = i + 2;
    if lower_keyword(t[i].text, &["function", "macro"]) && end > name {
        let args = (name + 1..end)
            .filter(|&j| t[j].span.start > t[j - 1].span.end)
            .count();
        stack.push(regions.len());
        regions.push(Region {
            name: t[name].text,
            start: i,
            body: end + 1,
            end: t.len(),
            args,
        });
        return;
    }
    let closes = lower_keyword(t[i].text, &["endfunction", "endmacro"]);
    if let Some(id) = closes.then(|| stack.pop()).flatten() {
        regions[id].end = end + 1;
    }
}

fn lua_function<'a>(syntax: PairedSyntax<'_, 'a>, i: usize) -> Option<Region<'a>> {
    let t = syntax.tokens;
    let mut open = i + 1;
    while open < t.len() && (t[open].kind == Kind::Word || matches!(t[open].text, "." | ":")) {
        open += 1;
    }
    let close = syntax.close_paren(open)?;
    let name = if open > i + 1 {
        t[open - 1].text
    } else if i >= 2 && t[i - 1].text == "=" {
        t[i - 2].text
    } else {
        "<anonymous>"
    };
    Some(Region {
        name,
        start: i,
        body: close + 1,
        end: t.len(),
        args: syntax.parameters(Lua, open + 1..close),
    })
}
fn lua_regions<'a>(syntax: PairedSyntax<'_, 'a>) -> Vec<Region<'a>> {
    let t = syntax.tokens;
    let mut scopes = Vec::new();
    let mut regions = Vec::new();
    for (i, token) in t.iter().enumerate() {
        match token.text {
            "function" => {
                let region = lua_function(syntax, i).map(|region| {
                    let index = regions.len();
                    regions.push(region);
                    index
                });
                scopes.push(region);
            }
            "if" | "do" | "repeat" => scopes.push(None),
            "end" | "until" => {
                if let Some(Some(id)) = scopes.pop() {
                    regions[id].end = i + 1;
                }
            }
            _ => {}
        }
    }
    regions
}

pub(in crate::domain::code::score) struct RegionSource<'s, 'text> {
    pub(in crate::domain::code::score) lang: Language,
    pub(in crate::domain::code::score) text: &'text str,
    pub(in crate::domain::code::score) syntax: PairedSyntax<'s, 'text>,
    pub(in crate::domain::code::score) facts: &'s Analysis,
}

impl<'text> RegionSource<'_, 'text> {
    pub(in crate::domain::code::score) fn regions(&self) -> Vec<Region<'text>> {
        match self.lang {
            CMake => cmake_regions(self.syntax),
            Lua => lua_regions(self.syntax),
            _ => self.declarations_and_literals(),
        }
    }

    fn declarations_and_literals(&self) -> Vec<Region<'text>> {
        let t = self.syntax.tokens;
        let lang = self.lang;
        let mut result = self.declared_regions();
        let declared = result.len();
        match lang {
            C | Cpp => self.append_written_c_bodies(&mut result),
            Go => result.extend(
                t.iter()
                    .enumerate()
                    .filter(|(_, token)| token.text == "func")
                    .filter_map(|(i, _)| anonymous_function(Go, self.syntax, i)),
            ),
            Js => append_js_literals(self.syntax, &mut result),
            _ => {}
        }
        if result.len() != declared {
            result.sort_unstable_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
        }
        result
    }

    fn append_written_c_bodies(&self, result: &mut Vec<Region<'text>>) {
        let syntax = self.syntax;
        let t = syntax.tokens;
        let lang = self.lang;
        // Navigation declarations depend on the translation unit. File scoring
        // also covers written function bodies in conditional compilation arms.
        let declared = result.len();
        let mut known = 0;
        let mut cursor = after_declarations(&result[..declared], &mut known, 1);
        while cursor + 2 < t.len() {
            let next = record_written_c_region(syntax, lang, cursor, result);
            cursor = after_declarations(&result[..declared], &mut known, next);
        }
    }
}

fn after_declarations(declarations: &[Region], known: &mut usize, mut cursor: usize) -> usize {
    while declarations
        .get(*known)
        .is_some_and(|region| region.start <= cursor)
    {
        cursor = cursor.max(declarations[*known].end);
        *known += 1;
    }
    cursor
}

fn record_written_c_region<'a>(
    syntax: PairedSyntax<'_, 'a>,
    lang: Language,
    i: usize,
    result: &mut Vec<Region<'a>>,
) -> usize {
    let Some(region) = written_c_region(syntax, lang, i) else {
        return i + 1;
    };
    let end = region.end;
    result.push(region);
    end
}

fn written_c_region<'a>(
    syntax: PairedSyntax<'_, 'a>,
    lang: Language,
    i: usize,
) -> Option<Region<'a>> {
    let PairedSyntax { tokens, pairs } = syntax;
    let (open, body, end) =
        crate::domain::code::cfamily::written_body::written_body(tokens, pairs, i, lang == Cpp)?;
    Some(Region {
        name: tokens[i].text,
        start: i,
        body,
        end,
        args: syntax.parameters(lang, open + 1..pairs[open]),
    })
}

fn append_js_literals<'a>(syntax: PairedSyntax<'_, 'a>, result: &mut Vec<Region<'a>>) {
    let t = syntax.tokens;
    // Anonymous function literals own their decisions independently of
    // the enclosing method or the file that initializes a callback registry.
    let mut expressions = script::ExpressionBodies::default();
    result.extend(
        t.iter()
            .enumerate()
            .filter_map(|(i, token)| match token.text {
                "function" => anonymous_function(Js, syntax, i),
                "=" if arrow_at(t, i) => Some(arrow_function(syntax, i, &mut expressions)),
                _ => None,
            }),
    );
}

fn arrow_at(t: &[Token], i: usize) -> bool {
    i > 0 && i + 2 < t.len() && t[i + 1].text == ">"
}

fn anonymous_function<'a>(
    lang: Language,
    syntax: PairedSyntax<'_, 'a>,
    start: usize,
) -> Option<Region<'a>> {
    let PairedSyntax { tokens: t, pairs } = syntax;
    let open = start + 1;
    let close = syntax.close_paren(open)?;
    let mut body = close + 1;
    if lang == Go {
        body = go_literal_body(t, pairs, body)?;
    }
    if at(t, body) == "{" && pairs[body] < t.len() {
        Some(Region {
            name: "<anonymous>",
            start,
            body,
            end: pairs[body] + 1,
            args: syntax.parameters(lang, open + 1..close),
        })
    } else {
        None
    }
}

fn go_literal_body(t: &[Token], pairs: &[usize], mut body: usize) -> Option<usize> {
    // A receiver followed by a method name and parameter list is a declaration.
    if t.get(body).is_some_and(|tok| tok.kind == Kind::Word) && at(t, body + 1) == "(" {
        return None;
    }
    let return_line = t[body - 1].line;
    while body < t.len()
        && t[body].line == return_line
        && !matches!(t[body].text, "{" | "}" | "," | ";" | "=" | ")" | "func")
    {
        if matches!(t[body].text, "(" | "[") && pairs[body] < t.len() {
            body = pairs[body];
        }
        body += 1;
    }
    Some(body)
}

fn arrow_function<'a>(
    syntax: PairedSyntax<'_, 'a>,
    i: usize,
    expressions: &mut script::ExpressionBodies,
) -> Region<'a> {
    let PairedSyntax { tokens: t, pairs } = syntax;
    let j = i - 1;
    let (start, args) = if t[j].text == ")" && pairs[j] < j {
        let open = pairs[j];
        (open, syntax.parameters(Js, open + 1..j))
    } else {
        (j, 1)
    };
    let name = if start >= 2 && t[start - 1].text == "=" {
        t[start - 2].text
    } else {
        "<arrow>"
    };
    let body = i + 2;
    let end = if t[body].text == "{" {
        pairs[body].min(t.len() - 1) + 1
    } else {
        expressions.end(t, pairs, body, |word| {
            matches!(word, "const" | "let" | "export" | "return" | "function")
        })
    };
    Region {
        name,
        start,
        body,
        end,
        args,
    }
}
