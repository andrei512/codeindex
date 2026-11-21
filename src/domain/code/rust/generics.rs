use super::*;

pub(super) struct Scope {
    end: usize,
    names: HashMap<String, usize>,
}

pub(super) fn enter(
    t: &[Token],
    pairs: &[usize],
    index: usize,
    open: usize,
    a: &mut Analysis,
    excluded: &mut [bool],
    scopes: &mut Vec<Scope>,
    members: &mut members::Members,
) {
    if word(t, open) != "<" {
        return;
    }
    let mut names = HashMap::new();
    let mut bounds = Vec::new();
    let mut depth = 1;
    let mut first = true;
    let mut i = open + 1;
    while i < t.len() && depth > 0 {
        if first && depth == 1 {
            let life = word(t, i) == "'";
            let constant = word(t, i) == "const";
            let at = i + usize::from(life || constant);
            let s = word(t, at);
            if ident(s) {
                let whole = if life {
                    format!("'{s}")
                } else {
                    name(s).to_string()
                };
                let start = if life { i } else { at };
                if !life && !constant {
                    let mut traits = Vec::new();
                    if word(t, at + 1) == ":" {
                        let mut start = at + 2;
                        loop {
                            let bound = members::path(t, start);
                            if bound.is_empty() {
                                break;
                            }
                            start += 1 + 3 * (bound.len() - 1);
                            if word(t, start) == "<" {
                                let mut nested = 1usize;
                                start += 1;
                                while start < t.len() && nested != 0 {
                                    match word(t, start) {
                                        "<" => nested += 1,
                                        ">" => nested -= 1,
                                        _ => {}
                                    }
                                    start += 1;
                                }
                            }
                            traits.push(bound);
                            if word(t, start) != "+" {
                                break;
                            }
                            start += 1;
                        }
                    }
                    bounds.push((whole.clone(), traits));
                }
                names.insert(whole, a.declarations.len());
                a.declarations.push(Declaration {
                    span: t[start].span.start..t[at].span.end,
                    kind: if life {
                        "lifetime_parameter"
                    } else {
                        "generic_parameter"
                    },
                });
                excluded[at] = true;
                if constant {
                    excluded[i] = true;
                }
            }
        }
        first = depth == 1 && word(t, i) == ",";
        match word(t, i) {
            "<" => depth += 1,
            ">" => depth -= 1,
            "{" | "(" | "[" if pairs[i] != usize::MAX => i = pairs[i],
            ";" => return,
            _ => {}
        }
        i += 1;
    }
    if depth != 0 || names.is_empty() {
        return;
    }
    let open = until(t, pairs, i, &["{", ";", "}"]);
    let end = if word(t, open) == "{" && pairs[open] != usize::MAX {
        pairs[open]
    } else {
        open
    };
    if end > index {
        for (name, traits) in bounds {
            let range = t[index].span.start..t.get(end).map_or(usize::MAX, |tok| tok.span.end);
            members
                .bounds
                .entry(name)
                .or_default()
                .push((range, traits));
        }
        scopes.push(Scope { end, names });
    }
}

pub(super) fn target(
    t: &[Token],
    i: usize,
    scopes: &[Scope],
) -> Option<(usize, std::ops::Range<usize>)> {
    let s = word(t, i);
    if !ident(s) {
        return None;
    }
    let life = i > 0 && word(t, i - 1) == "'";
    let key = if life {
        std::borrow::Cow::Owned(format!("'{s}"))
    } else {
        std::borrow::Cow::Borrowed(name(s))
    };
    for scope in scopes.iter().rev() {
        if let Some(&target) = scope.names.get(key.as_ref()) {
            return Some((target, t[i - usize::from(life)].span.start..t[i].span.end));
        }
    }
    None
}
pub(super) fn leave(scopes: &mut Vec<Scope>, i: usize) {
    while scopes.last().is_some_and(|s| s.end < i) {
        scopes.pop();
    }
}
