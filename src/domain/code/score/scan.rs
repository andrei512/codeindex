use super::duplicates::{self, Body};
use super::{Analysis, Language, Rule, Score, Token};
use Language::*;

#[derive(Default)]
struct Unit<'a> {
    name: &'a str,
    start: usize,
    end: usize,
    body: usize,
    args: usize,
    lines: usize,
    last: usize,
    branches: usize,
    bumps: usize,
    depth: usize,
    peak: usize,
    deep: bool,
    indents: Vec<usize>,
    suites: super::control::Suites,
}
impl Unit<'_> {
    fn depth(&mut self, n: usize) {
        self.peak = self.peak.max(n);
        self.bumps += usize::from(n >= 2 && !self.deep);
        self.deep = n >= 2;
    }
    fn finish(&self, score: &mut Score) {
        for (rule, value) in [
            (Rule::Complexity, self.branches + 1),
            (Rule::Bumps, self.bumps),
            (Rule::Nesting, self.peak),
            (Rule::Length, self.lines),
            (Rule::Arguments, self.args),
        ] {
            score.record(rule, value, self.start, self.name);
        }
    }
}
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
fn command(lang: Language) -> bool {
    matches!(
        lang,
        Shell
            | Powershell
            | CMake
            | Makefile
            | Batchfile
            | Dockerfile
            | Assembly
            | Sql
            | PlPgSql
            | Procfile
            | Mako
    )
}
fn branch(lang: Language, w: &str) -> bool {
    match lang {
        Html | Css | Proto => false,
        Lua => matches!(w, "if" | "elseif" | "for" | "while" | "until"),
        Sql | PlPgSql => ["when", "if", "elsif", "while", "for"]
            .iter()
            .any(|s| w.eq_ignore_ascii_case(s)),
        Assembly => matches!(
            w,
            "je" | "jne" | "jz" | "jnz" | "jg" | "jl" | "beq" | "bne" | "cbz" | "cbnz"
        ),
        CMake | Batchfile | Powershell => {
            ["if", "elseif", "foreach", "for", "while", "catch", "switch"]
                .iter()
                .any(|s| w.eq_ignore_ascii_case(s))
        }
        _ => matches!(
            w,
            "if" | "elif"
                | "elsif"
                | "elseif"
                | "unless"
                | "for"
                | "foreach"
                | "while"
                | "until"
                | "except"
                | "catch"
                | "case"
                | "when"
        ),
    }
}
fn lower_keyword(w: &str, options: &[&str]) -> bool {
    options.iter().any(|s| w.eq_ignore_ascii_case(s))
}
pub(super) fn at<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    t.get(i).map_or("", |x| x.text)
}
fn ind(text: &str, start: usize) -> usize {
    let pos = text[..start].rfind('\n').map_or(0, |n| n + 1);
    text.as_bytes()[pos..start]
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .map(|b| if *b == b'\t' { 8 } else { 1 })
        .sum()
}
fn parameters(lang: Language, t: &[Token], pairs: &[usize], open: usize) -> usize {
    let mut i = open + 1;
    let end = pairs[open];
    let mut count = 0;
    let mut occupied = false;
    let mut default_value = false;
    let type_arguments = matches!(
        lang,
        Rust | Cpp | Csharp | Java | Kotlin | Swift | Js | Dart | Hack | ObjectiveC
    );
    while i < end {
        match t[i].text {
            "," => {
                count += usize::from(occupied);
                occupied = false;
                default_value = false;
            }
            "(" | "[" | "{" => {
                occupied = true;
                i = pairs[i].min(end);
            }
            "<" if type_arguments && !default_value => {
                if let Some(close) = super::super::script::angle_end(&t[..end], i) {
                    i = close;
                }
                occupied = true;
            }
            "=" => default_value = true,
            "/" | "*" => {}
            _ => occupied = true,
        }
        i += 1;
    }
    count + usize::from(occupied)
}
struct Region<'a> {
    name: &'a str,
    start: usize,
    body: usize,
    end: usize,
    args: usize,
}
// Decorator lines immediately preceding a Python method can override binding.
fn static_method(t: &[Token], name: usize) -> bool {
    let mut p = name;
    while p > 0 && t[p - 1].line == t[name].line {
        p -= 1;
    }
    while p > 0 {
        let end = p;
        let line = t[p - 1].line;
        while p > 0 && t[p - 1].line == line {
            p -= 1;
        }
        if at(t, p) != "@" {
            break;
        }
        if t[p..end].iter().any(|tok| tok.text == "staticmethod") {
            return true;
        }
    }
    false
}
fn cmake_regions<'a>(t: &[Token<'a>], pairs: &[usize]) -> Vec<Region<'a>> {
    let mut regions = Vec::new();
    let mut stack = Vec::new();
    let mut i = 0;
    while i + 1 < t.len() {
        if t[i + 1].text != "(" || pairs[i + 1] >= t.len() {
            i += 1;
            continue;
        }
        let end = pairs[i + 1];
        if lower_keyword(t[i].text, &["function", "macro"]) && end > i + 2 {
            let name = i + 2;
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
        } else if lower_keyword(t[i].text, &["endfunction", "endmacro"])
            && let Some(id) = stack.pop()
        {
            regions[id].end = end + 1;
        }
        i = end + 1;
    }
    regions
}
fn lua_regions<'a>(t: &[Token<'a>], pairs: &[usize]) -> Vec<Region<'a>> {
    let mut scopes = Vec::new();
    let mut regions = Vec::new();
    for (i, token) in t.iter().enumerate() {
        match token.text {
            "function" => {
                let mut open = i + 1;
                while open < t.len()
                    && (t[open].kind == super::super::Kind::Word
                        || matches!(t[open].text, "." | ":"))
                {
                    open += 1;
                }
                let region = if at(t, open) == "(" && pairs[open] < t.len() {
                    let name = if open > i + 1 {
                        t[open - 1].text
                    } else if i >= 2 && t[i - 1].text == "=" {
                        t[i - 2].text
                    } else {
                        "<anonymous>"
                    };
                    let id = regions.len();
                    regions.push(Region {
                        name,
                        start: i,
                        body: pairs[open] + 1,
                        end: t.len(),
                        args: parameters(Lua, t, pairs, open),
                    });
                    Some(id)
                } else {
                    None
                };
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

fn regions<'a>(
    lang: Language,
    text: &'a str,
    t: &[Token<'a>],
    pairs: &[usize],
    facts: &Analysis,
) -> Vec<Region<'a>> {
    if lang == CMake {
        return cmake_regions(t, pairs);
    }
    if lang == Lua {
        return lua_regions(t, pairs);
    }
    let mut marks = Vec::new();
    for (id, d) in facts.declarations.iter().enumerate() {
        if !declaration(d.kind) {
            continue;
        }
        let start = t.partition_point(|x| x.span.end <= d.span.start);
        if start >= t.len() {
            continue;
        }
        marks.push((start, &text[d.span.clone()], id));
    }
    marks.sort_unstable_by_key(|x| x.0);
    marks.dedup_by_key(|x| x.0);
    let mut result = Vec::new();
    for (m, &(start, name, id)) in marks.iter().enumerate() {
        let next = marks.get(m + 1).map_or(t.len(), |m| m.0);
        let mut p = start + 1;
        while p < next
            && t[p].line <= t[start].line + 2
            && !matches!(t[p].text, "(" | "{" | ";" | ":" | "=")
        {
            p += 1;
        }
        let args = if at(t, p) == "(" && pairs[p] < t.len() {
            // C++ call operators spell the name with an empty pair of
            // parentheses followed by the actual parameter list.
            if lang == Cpp
                && name == "operator"
                && pairs[p] == p + 1
                && at(t, p + 2) == "("
                && pairs[p + 2] < t.len()
            {
                p += 2;
            }
            let mut n = parameters(lang, t, pairs, p);
            let receiver = match lang {
                Python => {
                    facts
                        .script
                        .as_ref()
                        .is_some_and(|s| s.has_method_receiver(id))
                        && !static_method(t, start)
                }
                Rust => t[p + 1..pairs[p]]
                    .iter()
                    .take_while(|tok| !matches!(tok.text, "," | ":"))
                    .any(|tok| tok.text == "self"),
                _ => false,
            };
            n = n.saturating_sub(usize::from(receiver));
            p = pairs[p] + 1;
            n
        } else {
            0
        };
        let mut body = p.min(next);
        let end = if matches!(lang, Python | Starlark) {
            while body < next && t[body].text != ":" {
                body += 1;
            }
            let base = ind(text, t[start].span.start);
            let mut j = (body + 1).min(t.len());
            while j < t.len() {
                if j > 0 && t[j].line > t[j - 1].line && ind(text, t[j].span.start) <= base {
                    break;
                }
                if matches!(t[j].text, "(" | "[" | "{") && pairs[j] < t.len() {
                    j = pairs[j];
                }
                j += 1;
            }
            j
        } else {
            while body < next
                && t[body].line <= t[start].line + 12
                && !matches!(t[body].text, "{" | ";")
            {
                body += 1;
            }
            if body < next && t[body].text == "{" && pairs[body] < t.len() {
                pairs[body] + 1
            } else {
                body = start;
                next
            }
        };
        if end > start {
            result.push(Region {
                name,
                start,
                body: body.min(end),
                end,
                args,
            });
        }
    }
    if matches!(lang, C | Cpp) {
        // Navigation declarations depend on the translation unit. File scoring
        // also covers written function bodies in conditional compilation arms.
        let mut known = result.iter().map(|r| (r.start, r.end)).peekable();
        let mut extra = Vec::new();
        let mut cursor = 1;
        while cursor + 2 < t.len() {
            while known.peek().is_some_and(|&(start, _)| start <= cursor) {
                let (_, end) = known.next().unwrap();
                cursor = cursor.max(end);
            }
            if cursor + 2 >= t.len() {
                break;
            }
            let i = cursor;
            cursor += 1;
            if t[i].kind != super::super::Kind::Word
                || at(t, i + 1) != "("
                || !t[i]
                    .text
                    .starts_with(|c: char| c.is_alphabetic() || c == '_')
                || matches!(
                    t[i].text,
                    "if" | "for"
                        | "while"
                        | "switch"
                        | "catch"
                        | "sizeof"
                        | "alignof"
                        | "decltype"
                        | "noexcept"
                        | "requires"
                        | "constexpr"
                        | "static_assert"
                )
            {
                continue;
            }
            if !(t[i - 1].kind == super::super::Kind::Word
                || matches!(t[i - 1].text, "*" | "&" | ">")
                || (i >= 2 && t[i - 2].text == ":" && t[i - 1].text == ":"))
                || matches!(
                    t[i - 1].text,
                    "return" | "else" | "do" | "case" | "new" | "define"
                )
            {
                continue;
            }
            let mut open = i + 1;
            if lang == Cpp
                && t[i].text == "operator"
                && pairs[open] == open + 1
                && at(t, open + 2) == "("
            {
                open += 2;
            }
            let close = pairs[open];
            if close >= t.len() {
                continue;
            }
            let mut body = close + 1;
            while matches!(at(t, body), "const" | "noexcept" | "override" | "final") {
                body += 1;
            }
            if at(t, body) != "{" || pairs[body] >= t.len() {
                continue;
            }
            let end = pairs[body] + 1;
            extra.push(Region {
                name: t[i].text,
                start: i,
                body,
                end,
                args: parameters(lang, t, pairs, open),
            });
            cursor = end;
        }
        result.extend(extra);
    }
    // Anonymous function literals own their decisions independently of
    // the enclosing method or the file that initializes a callback registry.
    if matches!(lang, Go | Js) {
        let keyword = if lang == Go { "func" } else { "function" };
        for (start, token) in t.iter().enumerate() {
            let open = start + 1;
            if token.text != keyword || at(t, open) != "(" || pairs[open] >= t.len() {
                continue;
            }
            let mut body = pairs[open] + 1;
            if lang == Go {
                // A receiver followed by a method name and parameter list is a declaration.
                if t.get(body)
                    .is_some_and(|tok| tok.kind == super::super::Kind::Word)
                    && at(t, body + 1) == "("
                {
                    continue;
                }
                let return_line = t[pairs[open]].line;
                while body < t.len()
                    && t[body].line == return_line
                    && !matches!(t[body].text, "{" | "}" | "," | ";" | "=" | ")" | "func")
                {
                    if matches!(t[body].text, "(" | "[") && pairs[body] < t.len() {
                        body = pairs[body];
                    }
                    body += 1;
                }
            }
            if at(t, body) == "{" && pairs[body] < t.len() {
                result.push(Region {
                    name: "<anonymous>",
                    start,
                    body,
                    end: pairs[body] + 1,
                    args: parameters(lang, t, pairs, open),
                });
            }
        }
    }
    if lang == Js {
        for i in 1..t.len().saturating_sub(2) {
            if t[i].text != "=" || t[i + 1].text != ">" {
                continue;
            }
            let j = i - 1;
            let (start, args) = if t[j].text == ")" && pairs[j] < j {
                let open = pairs[j];
                (open, parameters(lang, t, pairs, open))
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
                let mut j = body;
                while j < t.len() && !matches!(t[j].text, "," | ";" | ")" | "]" | "}") {
                    if matches!(t[j].text, "(" | "[" | "{") && pairs[j] < t.len() {
                        j = pairs[j];
                    }
                    j += 1;
                    if j < t.len()
                        && t[j].line > t[j - 1].line
                        && matches!(
                            t[j].text,
                            "const" | "let" | "export" | "return" | "function"
                        )
                    {
                        break;
                    }
                }
                j
            };
            result.push(Region {
                name,
                start,
                body,
                end,
                args,
            });
        }
    }
    result.sort_unstable_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    result
}
#[derive(PartialEq)]
enum CasePhase {
    Selector,
    Pattern,
    Commands,
}

pub(super) fn measure(
    lang: Language,
    text: &str,
    t: &[Token],
    pairs: &[usize],
    facts: &Analysis,
) -> Score {
    let mut regions = regions(lang, text, t, pairs, facts).into_iter().peekable();
    let mut score = Score::default();
    let mut stack = vec![Unit {
        name: "<file>",
        start: 1,
        end: t.len(),
        ..Unit::default()
    }];
    let mut bodies = Vec::new();
    let mut condition: Option<(usize, &str, usize)> = None;
    let finish_condition = |state: &mut Option<(usize, &str, usize)>, s: &mut Score| {
        if let Some((line, unit, count)) = state.take() {
            s.record(Rule::Conditionals, count, line, unit);
        }
    };
    let brace_controls = matches!(
        lang,
        Go | Rust | Js | Csharp | C | Cpp | Java | Kotlin | Swift | Dart | ObjectiveC | Php | Hack
    );
    let mut condition_end = 0;
    let mut continued_until = 0;
    let c_preprocessor = matches!(lang, C | Cpp | ObjectiveC);
    let mut directive_end = 0;
    // Each case tracks whether its next pattern has started. Quoted words
    // are single lexer tokens, so terminators inside quotes remain inert.
    let mut shell_patterns = Vec::new();
    for (i, token) in t.iter().enumerate() {
        while stack.len() > 1 && stack.last().unwrap().end <= i {
            finish(stack.pop().unwrap(), &mut score, &mut bodies);
        }
        while regions.peek().is_some_and(|r| r.start == i) {
            let r = regions.next().unwrap();
            stack.push(Unit {
                name: r.name,
                start: token.line,
                body: r.body,
                end: r.end,
                args: r.args,
                indents: if matches!(lang, Makefile | Mako) {
                    vec![ind(text, token.span.start)]
                } else {
                    Vec::new()
                },
                ..Unit::default()
            });
        }
        let first_source_line = i == 0 || t[i - 1].line != token.line;
        score.lines += usize::from(first_source_line);
        // Conditional compilation selects source. Only the written statements
        // inside its arms contribute to function length and runtime decisions.
        if c_preprocessor {
            if token.span.start < directive_end {
                continue;
            }
            if token.text == "#" && (i == 0 || t[i - 1].line < token.line) {
                directive_end = token.span.start;
                for line in text[directive_end..].split_inclusive('\n') {
                    directive_end += line.len();
                    if !line.trim_end_matches(['\r', '\n']).ends_with('\\') {
                        break;
                    }
                }
                continue;
            }
        }
        let u = stack.last_mut().unwrap();
        let first = first_source_line;
        let indentation_suites = matches!(lang, Python | Starlark);
        let statement = first && i >= continued_until;
        if indentation_suites && matches!(token.text, "(" | "[" | "{") && pairs[i] < t.len() {
            continued_until = continued_until.max(pairs[i] + 1);
        }
        if first {
            if i >= condition_end
                && !(indentation_suites && i < continued_until)
                && !(lang == Go
                    && i > 1
                    && matches!(t[i - 1].text, "&" | "|")
                    && t[i - 1].text == t[i - 2].text
                    && t[i - 2].span.end == t[i - 1].span.start)
            {
                finish_condition(&mut condition, &mut score);
            }
        }
        if u.last != token.line {
            u.lines += 1;
            u.last = token.line;
        }
        let w = token.text;
        let prev = i.checked_sub(1).map_or("", |j| t[j].text);
        let br = if indentation_suites {
            (statement || matches!(prev, ":" | ";"))
                && matches!(w, "if" | "elif" | "for" | "while" | "except" | "raise")
        } else if lang == Shell {
            let cmd_start = first
                || matches!(
                    prev,
                    ";" | "{" | "(" | ")" | "|" | "&" | "then" | "do" | "else"
                );
            if w == "case" && cmd_start {
                shell_patterns.push(CasePhase::Selector);
                false
            } else if w == "esac" && cmd_start {
                shell_patterns.pop();
                false
            } else if let Some(pattern) = shell_patterns.last_mut()
                && w == "in"
                && *pattern == CasePhase::Selector
            {
                *pattern = CasePhase::Pattern;
                false
            } else if let Some(pattern) = shell_patterns.last_mut()
                && w == ")"
                && *pattern == CasePhase::Pattern
            {
                *pattern = CasePhase::Commands;
                true
            } else {
                if let Some(pattern) = shell_patterns.last_mut()
                    && w == ";"
                    && matches!(at(t, i + 1), ";" | "&")
                    && t[i + 1].span.start == token.span.end
                {
                    *pattern = CasePhase::Pattern;
                }
                cmd_start && branch(lang, w)
            }
        } else if lang == CMake {
            at(t, i + 1) == "(" && branch(lang, w)
        } else if matches!(lang, Sql | PlPgSql) && prev.eq_ignore_ascii_case("end") {
            false
        } else {
            branch(lang, w)
                || c_preprocessor && w == "?"
                || lang == Swift && w == "guard"
                || lang == Rust && w == "=" && at(t, i + 1) == ">"
        };
        // Short-circuit operators and comprehension loops introduce independent
        // paths even when they occur outside a statement header.
        if indentation_suites
            && (matches!(w, "and" | "or")
                || w == "for" && !br
                || lang == Python && w == "else" && at(t, i + 1) == ":")
        {
            u.branches += 1;
        }
        if matches!(lang, Lua | Ruby | Perl | Php) && matches!(w, "and" | "or") {
            u.branches += 1;
        }
        if (brace_controls || matches!(lang, Ruby | Shell))
            && i >= u.body
            && (matches!(w, "&" | "|")
                && at(t, i + 1) == w
                && t[i + 1].span.start == token.span.end)
        {
            u.branches += 1;
        }
        if br {
            u.branches += 1;
            finish_condition(&mut condition, &mut score);
            condition = Some((token.line, u.name, 0));
            condition_end = if brace_controls
                && !matches!(lang, Go | Rust | Swift)
                && at(t, i + 1) == "("
                && pairs[i + 1] < u.end
            {
                pairs[i + 1]
            } else {
                0
            };
        }
        if let Some((_, _, count)) = &mut condition
            && (matches!(w, "and" | "or" | "AND" | "OR")
                || matches!(w, "&" | "|")
                    && at(t, i + 1) == w
                    && t[i + 1].span.start == token.span.end)
        {
            *count += 1;
        }
        if (condition_end != 0 && i == condition_end)
            || (i >= condition_end && matches!(w, ";" | "{" | ":"))
        {
            finish_condition(&mut condition, &mut score);
        }
        if indentation_suites {
            if statement {
                let indent = ind(text, token.span.start);
                while u.indents.last().is_some_and(|&parent| parent >= indent) {
                    u.indents.pop();
                }
                if matches!(w, "if" | "elif" | "else" | "for" | "while")
                    || w == "async" && at(t, i + 1) == "for"
                {
                    u.indents.push(indent);
                }
                u.depth(u.indents.len());
            }
        } else if matches!(lang, Makefile | Mako) {
            if first {
                let n = ind(text, token.span.start);
                while u.indents.last().is_some_and(|&x| x >= n) {
                    u.indents.pop();
                }
                u.indents.push(n);
                u.depth(
                    u.indents
                        .len()
                        .saturating_sub(if u.name == "<file>" { 1 } else { 2 }),
                );
            }
        } else if brace_controls {
            let depth = u.suites.depth(lang, t, pairs, i, u.end);
            u.depth(depth);
        } else {
            let (open, close) = match lang {
                Html => (
                    w == "<"
                        && t.get(i + 1)
                            .is_some_and(|n| n.kind == super::super::Kind::Word)
                        && !void_html(at(t, i + 1)),
                    w == "<" && at(t, i + 1) == "/" && !void_html(at(t, i + 2))
                        || w == "/" && at(t, i + 1) == ">",
                ),
                Shell => (
                    matches!(w, "then" | "do" | "case" | "{"),
                    matches!(w, "fi" | "done" | "esac" | "}") || w == "elif",
                ),
                Ruby if matches!(w, "if" | "unless" | "while" | "until") => (
                    first || matches!(prev, ";" | "=" | "then" | "else" | "("),
                    false,
                ),
                Lua if w == "function" && i < u.body => (false, false),
                Ruby | Lua => (
                    matches!(
                        w,
                        "if" | "for"
                            | "while"
                            | "unless"
                            | "case"
                            | "begin"
                            | "function"
                            | "def"
                            | "repeat"
                    ),
                    matches!(w, "end" | "until"),
                ),
                CMake => (
                    lower_keyword(w, &["if", "while", "foreach", "function", "macro"]),
                    lower_keyword(
                        w,
                        &["endif", "endwhile", "endforeach", "endfunction", "endmacro"],
                    ),
                ),
                Sql | PlPgSql => (
                    !prev.eq_ignore_ascii_case("end")
                        && (lower_keyword(w, &["begin", "case"])
                            || lang == PlPgSql && lower_keyword(w, &["if", "loop"])),
                    w.eq_ignore_ascii_case("end"),
                ),
                _ => (w == "{", w == "}"),
            };
            if open {
                u.depth += 1;
            }
            if close {
                u.depth = u.depth.saturating_sub(1);
            }
            let d = u.depth.saturating_sub(usize::from(
                u.name != "<file>" && !command(lang) && !matches!(lang, Ruby | Lua),
            ));
            u.depth(d);
        }
    }
    finish_condition(&mut condition, &mut score);
    while let Some(mut u) = stack.pop() {
        // A collection of declarations has no method length. Keep actual
        // module-level executable control flow and structural document metrics.
        if u.name == "<file>"
            && (u.branches == 0 || matches!(lang, Go | Java | Rust | C | Cpp | ObjectiveC))
            && brace_controls
            && (!matches!(lang, Js | Php | Swift | Kotlin | Csharp)
                || !facts.declarations.is_empty())
        {
            u.lines = 0;
        }
        finish(u, &mut score, &mut bodies);
    }
    duplicates::record(&mut score, t, &bodies);
    score
}
fn void_html(s: &str) -> bool {
    lower_keyword(
        s,
        &[
            "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
            "source", "track", "wbr",
        ],
    )
}
fn finish<'a>(u: Unit<'a>, score: &mut Score, bodies: &mut Vec<Body<'a>>) {
    u.finish(score);
    if u.name != "<file>" && u.lines >= Rule::Duplication.threshold() && u.body < u.end {
        bodies.push(Body {
            name: u.name,
            line: u.start,
            lines: u.lines,
            tokens: u.body..u.end,
        });
    }
}
