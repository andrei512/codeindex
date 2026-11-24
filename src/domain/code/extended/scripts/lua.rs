use super::{Analysis, Token, before, call, declare, word};
use std::collections::HashMap;

#[derive(Default)]
struct Scope<'a> {
    owner: Option<usize>,
    bindings: HashMap<&'a str, Option<usize>>,
}

// A local's scope begins after its initializer. The repeat block, conversely,
// remains visible throughout its until-expression. Find that boundary without
// consuming the tokens the main loop still needs for calls and declarations.
fn expression_end(t: &[Token<'_>], start: usize) -> usize {
    let mut p = start;
    let mut atom = true;
    while p < t.len() {
        let text = t[p].text;
        if atom && matches!(text, "not" | "-" | "#" | "~") {
            p += 1;
            continue;
        }
        if matches!(text, "(" | "{" | "[") {
            let mut depth = 1usize;
            p += 1;
            while p < t.len() && depth > 0 {
                match word(t, p) {
                    "(" | "{" | "[" => depth += 1,
                    ")" | "}" | "]" => depth -= 1,
                    _ => {}
                }
                p += 1;
            }
        } else if atom && text == "function" {
            let mut depth = 1usize;
            p += 1;
            while p < t.len() && depth > 0 {
                match word(t, p) {
                    "function" | "if" | "do" => depth += 1,
                    "end" => depth -= 1,
                    _ => {}
                }
                p += 1;
            }
        } else if atom && !matches!(text, "end" | "until" | "else" | ";" | ")") {
            p += 1;
            if text.starts_with(|c: char| c.is_ascii_digit()) {
                while p < t.len()
                    && t[p].at == t[p - 1].span().end
                    && t[p].text.starts_with(|c: char| c.is_ascii_alphanumeric())
                {
                    p += 1;
                }
            }
        } else if !atom
            && matches!(
                text,
                "," | "and"
                    | "or"
                    | ":"
                    | "."
                    | "+"
                    | "-"
                    | "*"
                    | "/"
                    | "%"
                    | "^"
                    | "&"
                    | "|"
                    | "~"
                    | "<"
                    | ">"
                    | "="
            )
        {
            atom = true;
            // Adjacent punctuation may form a compound binary operator.
            let op = text;
            p += 1;
            while p < t.len()
                && t[p].at == t[p - 1].span().end
                && matches!(
                    (op, t[p].text),
                    (".", ".")
                        | ("=", "=")
                        | ("<", "=" | "<")
                        | (">", "=" | ">")
                        | ("~", "=")
                        | ("/", "/")
                )
            {
                p += 1;
            }
            continue;
        } else if !atom && text.starts_with(['\'', '"']) {
            p += 1;
        } else {
            break;
        }
        atom = false;
    }
    p
}

enum Change<'a> {
    Hide(usize, Vec<&'a str>),
    EndRepeat,
}

pub(super) fn analyze(t: &[Token<'_>]) -> Analysis {
    let mut out = Analysis::default();
    let mut scopes = vec![Scope::default()];
    let mut changes: HashMap<usize, Vec<Change<'_>>> = HashMap::new();
    let mut loop_names = HashMap::new();
    let mut i = 0;
    while i < t.len() {
        if let Some(ready) = changes.remove(&i) {
            for change in ready {
                match change {
                    Change::Hide(depth, names) => {
                        if let Some(scope) = scopes.get_mut(depth) {
                            for name in names {
                                scope.bindings.insert(name, None);
                            }
                        }
                    }
                    Change::EndRepeat if scopes.len() > 1 => {
                        scopes.pop();
                    }
                    Change::EndRepeat => {}
                }
            }
        }
        let tok = &t[i];
        let owner = scopes.last().unwrap().owner;
        match tok.text {
            "function" => {
                let assigned = before(t, i) == "=" && i >= 2 && t[i - 2].name();
                let mut n = if assigned { i - 2 } else { i + 1 };
                let mut body = Scope::default();
                if t.get(n).is_some_and(|x| x.name()) {
                    while !assigned
                        && matches!(word(t, n + 1), "." | ":")
                        && t.get(n + 2).is_some_and(|x| x.name())
                    {
                        n += 2;
                    }
                    if assigned || word(t, n + 1) == "(" {
                        let id = declare(&mut out, t[n].span(), "function_definition");
                        body.owner = Some(id);
                        if n == i + 1 && before(t, i) == "local" {
                            scopes
                                .last_mut()
                                .unwrap()
                                .bindings
                                .insert(t[n].text, Some(id));
                        }
                        if !assigned {
                            i = n;
                        }
                    }
                }
                if word(t, i + 1) == "(" {
                    i += 1;
                    while i + 1 < t.len() && word(t, i + 1) != ")" {
                        i += 1;
                        if t[i].name() {
                            body.bindings.insert(t[i].text, None);
                        }
                    }
                }
                scopes.push(body);
            }
            "for" | "local" if word(t, i + 1) != "function" => {
                let mut names = Vec::new();
                let mut p = i + 1;
                while t.get(p).is_some_and(|token| token.name()) {
                    let name = t[p].text;
                    if matches!(
                        scopes.iter().rev().find_map(|s| s.bindings.get(name)),
                        Some(Some(_))
                    ) {
                        names.push(name);
                    }
                    p += 1;
                    if word(t, p) == "<" && word(t, p + 2) == ">" {
                        p += 3;
                    }
                    if word(t, p) != "," {
                        break;
                    }
                    p += 1;
                }
                if names.is_empty() && p > i + 1 {
                    i = p;
                    continue;
                }
                if tok.text == "local" {
                    let stop = if word(t, p) == "=" {
                        expression_end(t, p + 1)
                    } else {
                        p
                    };
                    changes
                        .entry(stop)
                        .or_default()
                        .push(Change::Hide(scopes.len() - 1, names));
                } else if matches!(word(t, p), "=" | "in") {
                    let stop = expression_end(t, p + 1);
                    if word(t, stop) == "do" {
                        loop_names.insert(stop, names);
                    }
                }
                if p > i + 1 {
                    i = p;
                    continue;
                }
            }
            "do" | "then" | "repeat" => {
                let mut block = Scope {
                    owner,
                    ..Scope::default()
                };
                if let Some(names) = loop_names.remove(&i) {
                    for name in names {
                        block.bindings.insert(name, None);
                    }
                }
                scopes.push(block);
            }
            "else" | "elseif" => {
                if scopes.len() > 1 {
                    scopes.pop();
                }
                if tok.text == "else" {
                    scopes.push(Scope {
                        owner,
                        ..Scope::default()
                    });
                }
            }
            "until" => {
                let stop = expression_end(t, i + 1);
                changes.entry(stop).or_default().push(Change::EndRepeat);
            }
            "end" => {
                if scopes.len() > 1 {
                    scopes.pop();
                }
            }
            _ if tok.name() && !matches!(before(t, i), "." | ":") => {
                if word(t, i + 1) == "(" {
                    if let Some(Some(id)) =
                        scopes.iter().rev().find_map(|s| s.bindings.get(tok.text))
                    {
                        call(&mut out, tok.span(), owner, *id);
                    }
                } else if word(t, i + 1) == "="
                    && let Some(binding) = scopes
                        .iter_mut()
                        .rev()
                        .find_map(|s| s.bindings.get_mut(tok.text))
                {
                    *binding = None;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}
