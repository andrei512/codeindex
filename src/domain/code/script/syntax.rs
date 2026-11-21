use super::{
    Analysis, Binding, Declaration, Evidence, Language, Lookup, Token, add_use, tok, word,
};
pub(super) fn paired(t: &[Token]) -> Vec<usize> {
    let mut stack = Vec::new();
    let mut end = vec![t.len(); t.len()];
    for (i, token) in t.iter().enumerate() {
        match token.text {
            "(" | "[" | "{" => stack.push(i),
            ")" | "]" | "}" => {
                if let Some(&j) = stack.last() {
                    if matches!(
                        (t[j].text, token.text),
                        ("(", ")") | ("[", "]") | ("{", "}")
                    ) {
                        stack.pop();
                        end[j] = i;
                        end[i] = j;
                    }
                }
            }
            _ => {}
        }
    }
    end
}
pub(super) fn bind_token(
    t: &Token,
    scope: usize,
    kind: &'static str,
    a: &mut Analysis,
    e: &mut Evidence,
) -> usize {
    let id = a.declarations.len();
    a.declarations.push(Declaration {
        span: t.span.clone(),
        kind,
    });
    e.declare(scope, t.text, Binding::Local(id));
    id
}
pub(super) fn pattern(
    t: &[Token],
    end: &[usize],
    open: usize,
    scope: usize,
    kind: &'static str,
    a: &mut Analysis,
    e: &mut Evidence,
) {
    let mut pending = vec![open];
    while let Some(begin) = pending.pop() {
        let mut p = begin + 1;
        let limit = end[begin].min(t.len());
        let mut first = true;
        while p < limit {
            match tok(t, p) {
                "," => first = true,
                "." | "*" => {}
                "{" | "[" if first => {
                    pending.push(p);
                    p = end[p];
                    first = false;
                }
                "(" | "[" | "{" => {
                    p = end[p];
                    first = false;
                }
                _ if first && word(&t[p]) => {
                    if tok(t, begin) == "{" && tok(t, p + 1) == ":" {
                        p += 1;
                    } else {
                        bind_token(&t[p], scope, kind, a, e);
                        first = false;
                    }
                }
                _ => first = false,
            }
            p += 1;
        }
    }
}
pub(super) fn parameters(
    t: &[Token],
    end: &[usize],
    mut p: usize,
    close: usize,
    scope: usize,
    a: &mut Analysis,
    e: &mut Evidence,
) {
    let first_param = p;
    let mut pattern_bindings = None;
    let mut first = true;
    while p < close {
        match tok(t, p) {
            "," => first = true,
            "<" if !first => {
                if let Some(stop) = angle_end(t, p) {
                    p = stop;
                }
            }
            "{" | "[" if first => {
                let from = a.declarations.len();
                pattern(t, end, p, scope, "parameter", a, e);
                if from < a.declarations.len() {
                    e.destructured_parameters.push(from..a.declarations.len());
                }
                if p == first_param {
                    pattern_bindings = Some((from, a.declarations.len()));
                }
                p = end[p];
                first = false;
            }
            "(" | "[" | "{" => {
                p = end[p];
                first = false;
            }
            "*" | "/" | "." | "readonly" | "public" | "private" | "protected" => {}
            _ if first && word(&t[p]) => {
                let id = bind_token(&t[p], scope, "parameter", a, e);
                if tok(t, p + 1) == ":" && t.get(p + 2).is_some_and(word) {
                    e.types.insert(id, (scope, t[p + 2].span.clone()));
                }
                first = false;
            }
            _ => first = false,
        }
        p += 1;
    }
    let after = if matches!(tok(t, first_param), "{" | "[") {
        end[first_param] + 1
    } else {
        first_param + 1
    };
    if after + 1 < close && tok(t, after) == ":" && tok(t, after + 1) == "{" {
        let shape = e.scopes.len();
        e.scopes.push(Default::default());
        let mut j = after + 2;
        let stop = end[after + 1].min(close);
        while j < stop {
            if word(&t[j])
                && (tok(t, j + 1) == ":" || tok(t, j + 1) == "?" && tok(t, j + 2) == ":")
                && (j == after + 2
                    || matches!(tok(t, j - 1), "," | ";" | "readonly")
                    || t[j - 1].line < t[j].line)
            {
                let id = a.declarations.len();
                e.members.insert((shape, t[j].text.into()), id);
                a.declarations.push(Declaration {
                    span: t[j].span.clone(),
                    kind: "local_binding",
                });
            }
            if matches!(tok(t, j), "(" | "[" | "{") {
                j = end[j];
            }
            j += 1;
        }
        if let Some(owner) = e.scopes[scope].owner {
            e.props.insert(owner, shape);
        }
        if let Some((from, to)) = pattern_bindings {
            for id in from..to {
                let start = a.declarations[id].span.start;
                let idx =
                    t[first_param..after].partition_point(|x| x.span.start < start) + first_param;
                if let Some(&target) = e.members.get(&(shape, t[idx].text.to_owned())) {
                    add_use(a, e, &t[idx], scope, false, Lookup::Known(target));
                }
            }
        } else if let Some(Binding::Local(id)) = e.scopes[scope].bindings.get(tok(t, first_param)) {
            e.shapes.insert(*id, shape);
        }
    }
}
pub(super) fn angle_end(t: &[Token], mut p: usize) -> Option<usize> {
    if tok(t, p) != "<" {
        return None;
    }
    let mut depth = 1;
    p += 1;
    while p < t.len() {
        match tok(t, p) {
            "<" => depth += 1,
            ">" if tok(t, p - 1) != "=" => {
                depth -= 1;
                if depth == 0 {
                    return Some(p);
                }
            }
            _ => {}
        }
        if t[p].line > t[p - 1].line + 1 {
            return None;
        }
        p += 1;
    }
    None
}
pub(super) fn args(t: &[Token], mut p: usize) -> Option<usize> {
    if tok(t, p) == "<" {
        p = angle_end(t, p)? + 1;
    }
    (tok(t, p) == "(").then_some(p)
}
pub(super) fn method_body(t: &[Token], end: &[usize], close: usize) -> Option<usize> {
    if close >= t.len() {
        return None;
    }
    let mut p = close + 1;
    if tok(t, p) == ":" {
        p += 1;
        if tok(t, p) == "{" {
            p = end[p] + 1;
        }
        while p < t.len() {
            match tok(t, p) {
                "{" => break,
                ";" | "}" | "," | ")" => return None,
                "=" if tok(t, p + 1) != ">" => return None,
                "(" | "[" => p = end[p],
                "<" => p = angle_end(t, p)?,
                _ => {}
            }
            p += 1;
        }
    }
    (tok(t, p) == "{").then_some(p)
}
pub(super) fn expr_end(t: &[Token], end: &[usize], mut p: usize) -> usize {
    let first = p;
    while p < t.len() {
        if matches!(tok(t, p), "," | ")" | "]" | "}" | ";") {
            break;
        }
        if p > first
            && t[p - 1].line < t[p].line
            && matches!(
                tok(t, p),
                "const" | "let" | "function" | "return" | "export"
            )
        {
            break;
        }
        if tok(t, p) == "<"
            && t.get(p + 1).is_some_and(word)
            && let Some(last) = angle_end(t, p)
            && matches!(tok(t, last + 1), "(" | "=" | ")" | "," | ";")
        {
            p = last;
        } else if matches!(tok(t, p), "(" | "[" | "{") {
            p = end[p];
        }
        p += 1;
    }
    p
}

/// A signature introduces type parameters before the argument/return types use them.
pub(super) fn signature(
    t: &[Token],
    end: &[usize],
    name: usize,
    paren: usize,
    body: usize,
    scope: usize,
    a: &mut Analysis,
    e: &mut Evidence,
) {
    if paren >= t.len() || end[paren] >= t.len() {
        return;
    }
    let mut declared = std::collections::HashSet::new();
    if tok(t, name + 1) == "<" {
        let mut first = true;
        let mut p = name + 2;
        while p < paren.saturating_sub(1) {
            match tok(t, p) {
                "," => first = true,
                "const" | "out" | "in" if first => {}
                _ if first && word(&t[p]) => {
                    bind_token(&t[p], scope, "local_binding", a, e);
                    declared.insert(p);
                    first = false;
                }
                "<" => {
                    if let Some(stop) = angle_end(t, p) {
                        p = stop;
                    }
                }
                "(" | "{" | "[" => p = end[p],
                _ => {}
            }
            p += 1;
        }
    }
    let ret = end[paren] + 1;
    if tok(t, ret) == ":"
        && ret + 1 < body
        && let Some(owner) = e.scopes[scope].owner
    {
        let p = ret + 1;
        if tok(t, p) == "{" {
            let shape = e.scopes.len();
            e.scopes.push(Default::default());
            let mut j = p + 1;
            while j < end[p].min(body) {
                let field = word(&t[j])
                    && (j == p + 1
                        || matches!(tok(t, j - 1), "," | ";" | "readonly")
                        || t[j - 1].line < t[j].line)
                    && matches!(tok(t, j + 1), ":" | "?" | "(");
                if field {
                    let id = a.declarations.len();
                    e.members.insert((shape, t[j].text.to_owned()), id);
                    a.declarations.push(Declaration {
                        span: t[j].span.clone(),
                        kind: "local_binding",
                    });
                    if tok(t, j + 1) == ":" && t.get(j + 2).is_some_and(word) {
                        e.types.insert(id, (scope, t[j + 2].span.clone()));
                    }
                }
                if matches!(tok(t, j), "(" | "[" | "{") {
                    j = end[j];
                }
                j += 1;
            }
            e.annotated_shapes.insert(owner, shape);
        } else if word(&t[p]) && !super::keyword(Language::Js, t[p].text) {
            e.annotated_returns
                .insert(owner, (scope, t[p].span.clone()));
        }
    }
    let arg_end = end[paren];
    let mut annotation = false;
    let mut depth = 0;
    for p in name + 1..body.min(t.len()) {
        let tag = tok(t, p);
        if p == paren {
            depth = 0;
            annotation = false;
            continue;
        }
        if paren < p && p < arg_end {
            if tag == ":" && depth == 0 {
                annotation = true;
            }
            if tag == "," && depth == 0 {
                annotation = false;
            }
            if matches!(tag, "{" | "(" | "[") {
                depth += 1;
            }
            if matches!(tag, "}" | ")" | "]") {
                depth -= 1;
            }
            if !annotation {
                continue;
            }
        }
        if word(&t[p])
            && !super::keyword(Language::Js, tag)
            && !declared.contains(&p)
            && !(matches!(tok(t, p + 1), ":" | "?") || p > 0 && tok(t, p - 1) == ".")
        {
            add_use(
                a,
                e,
                &t[p],
                scope,
                false,
                Lookup::Name {
                    scope,
                    receiver: None,
                },
            );
        }
    }
}
