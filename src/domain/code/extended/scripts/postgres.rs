use super::{Analysis, Language, Token, declare, tokens, usage, word};
use std::ops::Range;

fn name(tok: Token<'_>) -> Option<Range<usize>> {
    if tok.name() {
        Some(tok.span())
    } else if tok.text.starts_with('"') && tok.text.ends_with('"') && tok.text.len() > 2 {
        Some(tok.at + 1..tok.span().end - 1)
    } else {
        None
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
fn labels(t: &[Token<'_>], out: &mut Analysis, owner: Option<usize>) {
    let mut blocks = Vec::<Block>::new();
    let mut pending = None;
    let mut i = 0;
    while i < t.len() {
        let tok = t[i];
        i += 1;
        if tok.text == "<"
            && word(t, i) == "<"
            && word(t, i + 2) == ">"
            && word(t, i + 3) == ">"
            && let Some(span) = t.get(i + 1).and_then(|n| name(*n))
            && matches!(
                word(t, i + 4).to_ascii_lowercase().as_str(),
                "declare" | "begin" | "loop" | "while" | "for" | "foreach"
            )
        {
            pending = Some((key(t[i + 1]), declare(out, span, "label")));
            i += 4;
        } else if tok.text.eq_ignore_ascii_case("begin") || tok.text.eq_ignore_ascii_case("loop") {
            blocks.push(Block {
                label: pending.take(),
                is_loop: tok.text.eq_ignore_ascii_case("loop"),
            });
        } else if tok.text.eq_ignore_ascii_case("if") || tok.text.eq_ignore_ascii_case("case") {
            blocks.push(Block {
                label: None,
                is_loop: false,
            });
        } else if tok.text.eq_ignore_ascii_case("exit") || tok.text.eq_ignore_ascii_case("continue")
        {
            if let Some(&next) = t.get(i)
                && let Some(span) = name(next)
            {
                let wanted = key(next);
                if let Some(b) = blocks
                    .iter()
                    .rev()
                    .find(|b| b.label.as_ref().is_some_and(|(k, _)| k == &wanted))
                    && (b.is_loop || tok.text.eq_ignore_ascii_case("exit"))
                {
                    usage(out, span, owner, b.label.as_ref().unwrap().1, false);
                }
            }
        } else if tok.text.eq_ignore_ascii_case("end") {
            let block = blocks.pop();
            if matches!(
                word(t, i).to_ascii_lowercase().as_str(),
                "loop" | "if" | "case"
            ) {
                i += 1;
            }
            if let Some((k, id)) = block.and_then(|b| b.label)
                && let Some(&next) = t.get(i)
                && let Some(span) = name(next)
                && key(next) == k
            {
                usage(out, span, owner, id, false);
            }
        }
    }
}
fn routines(source: &str, t: &[Token<'_>], out: &mut Analysis, mixed: bool) {
    let mut i = 0;
    while i < t.len() {
        let tok = t[i];
        i += 1;
        if !tok.text.eq_ignore_ascii_case("create") {
            continue;
        }
        let mut j = i;
        if word(t, j).eq_ignore_ascii_case("or") && word(t, j + 1).eq_ignore_ascii_case("replace") {
            j += 2;
        }
        let kind = if word(t, j).eq_ignore_ascii_case("function") {
            "function_definition"
        } else if word(t, j).eq_ignore_ascii_case("procedure") {
            "procedure_declaration"
        } else {
            continue;
        };
        j += 1;
        let Some(mut span) = t.get(j).and_then(|n| name(*n)) else {
            continue;
        };
        while word(t, j + 1) == "."
            && let Some(part) = t.get(j + 2).and_then(|n| name(*n))
        {
            j += 2;
            span = part;
        }
        if word(t, j + 1) != "(" {
            continue;
        }
        let raw_span = t[j].span();
        let mut depth = 0usize;
        let mut plpgsql = false;
        let mut body = None;
        let mut end = j + 1;
        while end < t.len() && t[end].text != ";" {
            match t[end].text {
                "(" => depth += 1,
                ")" => depth = depth.saturating_sub(1),
                keyword
                    if depth == 0
                        && keyword.eq_ignore_ascii_case("language")
                        && !word(t, end - 1).eq_ignore_ascii_case("set")
                        && !word(t, end - 1).eq_ignore_ascii_case("returns") =>
                {
                    plpgsql = word(t, end + 1).eq_ignore_ascii_case("plpgsql")
                        || matches!(word(t, end + 1), "\"plpgsql\"" | "'plpgsql'");
                }
                keyword if depth == 0 && keyword.eq_ignore_ascii_case("as") => {
                    body = t.get(end + 1).copied();
                }
                _ => {}
            }
            end += 1;
        }
        i = end;
        if mixed && !plpgsql {
            continue;
        }
        let kind = match (mixed, kind) {
            (true, "function_definition") => "plpgsql_function",
            (true, _) => "plpgsql_procedure",
            (_, kind) => kind,
        };
        let owner = if mixed
            && let Some(id) = out.declarations.iter().position(|d| {
                d.kind == "object_reference" && (d.span == raw_span || d.span == span)
            }) {
            out.declarations[id].span = span;
            out.declarations[id].kind = kind;
            id
        } else {
            declare(out, span, kind)
        };
        if plpgsql
            && let Some(body) = body
            && body.text.starts_with('$')
            && let Some(k) = body.text[1..].find('$')
        {
            let width = k + 2;
            if body.text.len() >= 2 * width && body.text.ends_with(&body.text[..width]) {
                let base = body.at + width;
                let mut inner = tokens(&source[base..body.span().end - width], Language::PlPgSql);
                for token in &mut inner {
                    token.at += base;
                }
                labels(&inner, out, Some(owner));
            }
        }
    }
}

pub(super) fn sql_routines(source: &str, out: &mut Analysis) {
    let t = tokens(source, Language::PlPgSql);
    // The outer SQL parser emits declarations only. Its generic lexer can see
    // CREATE text inside a PostgreSQL dollar body or a nested comment.
    out.declarations.retain(|d| {
        d.kind != "object_reference" || t.binary_search_by_key(&d.span.start, |tok| tok.at).is_ok()
    });
    routines(source, &t, out, true);
}

pub(super) fn analyze(source: &str, t: &[Token<'_>]) -> Analysis {
    let mut out = Analysis::default();
    routines(source, t, &mut out, false);
    labels(t, &mut out, None);
    out
}
