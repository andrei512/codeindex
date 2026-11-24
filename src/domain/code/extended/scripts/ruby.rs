use super::{Analysis, Token, before, call, colon_name, declare, word};
use std::collections::{HashMap, HashSet};

pub(super) fn analyze(source: &str, t: &[Token<'_>]) -> Analysis {
    let mut out = Analysis::default();
    let mut owner = None;
    let mut class = None;
    let mut frames = Vec::new();
    let mut methods = HashMap::new();
    let mut constants = HashSet::new();
    let mut rebound = HashSet::new();
    let mut pending = Vec::new();
    for (i, tok) in t.iter().enumerate() {
        let prev = before(t, i);
        if matches!(prev, "." | ":" | "@" | "$") {
            continue;
        }
        let start = i == 0 || source[t[i - 1].span().end..tok.at].contains('\n') || prev == ";";
        match tok.text {
            "end" => {
                if let Some((old_class, old_owner)) = frames.pop() {
                    (class, owner) = (old_class, old_owner);
                }
            }
            "class" | "module" => {
                let top = frames.is_empty();
                frames.push((class, owner));
                owner = None;
                class = None;
                if let Some(n) = colon_name(t, i + 1)
                    && t[n].text.starts_with(char::is_uppercase)
                {
                    declare(
                        &mut out,
                        t[n].span(),
                        if tok.text == "class" {
                            "class_definition"
                        } else {
                            "module_declaration"
                        },
                    );
                    if top && n == i + 1 {
                        constants.insert(t[n].text);
                    } else {
                        rebound.insert(t[n].text);
                    }
                    if top && n == i + 1 {
                        class = Some(t[n].text);
                    }
                }
            }
            "def" => {
                let Some(mut n) = colon_name(t, i + 1) else {
                    continue;
                };
                let mut receiver = None;
                if word(t, n + 1) == "." && t.get(n + 2).is_some_and(|x| x.name()) {
                    receiver = if n == i + 1 && t[n].text == "self" {
                        class
                    } else if frames.is_empty()
                        && n == i + 1
                        && t[n].text.starts_with(char::is_uppercase)
                    {
                        Some(t[n].text)
                    } else {
                        None
                    };
                    n += 2;
                }
                let mut span = t[n].span();
                if matches!(word(t, n + 1), "!" | "?" | "=") && t[n + 1].at == span.end {
                    n += 1;
                    span.end = t[n].span().end;
                }
                let name = &source[span.clone()];
                let id = declare(&mut out, span, "method_definition");
                if let Some(c) = receiver {
                    methods
                        .entry((c, name))
                        .and_modify(|slot| *slot = None)
                        .or_insert(Some(id));
                }
                // Endless methods have no closing end. Skip ownership of their
                // expression rather than leaking a method into later statements.
                let mut j = n + 1;
                if word(t, j) == "(" {
                    let mut level = 0;
                    while j < t.len() {
                        match t[j].text {
                            "(" => level += 1,
                            ")" => level -= 1,
                            _ => {}
                        }
                        j += 1;
                        if level == 0 {
                            break;
                        }
                    }
                }
                if word(t, j) != "=" {
                    frames.push((class, owner));
                    owner = Some(id);
                }
            }
            "do" | "case" | "begin" | "for" => frames.push((class, owner)),
            "if" | "unless" | "while" | "until" if start || matches!(prev, "=" | "(" | "=>") => {
                frames.push((class, owner))
            }
            _ if tok.text.starts_with(char::is_uppercase) => {
                if word(t, i + 1) == "=" {
                    rebound.insert(tok.text);
                }
                if prev != "def" && word(t, i + 1) == "." && t.get(i + 2).is_some_and(|x| x.name())
                {
                    let n = i + 2;
                    let mut span = t[n].span();
                    let mut j = n + 1;
                    if matches!(word(t, j), "!" | "?") && t[j].at == span.end {
                        span.end += 1;
                        j += 1;
                    }
                    if word(t, j) != "=" || t.get(j).is_some_and(|next| source[span.end..next.at].contains('\n')) {
                        pending.push((tok.text, span, owner));
                    }
                }
            }
            _ => {}
        }
    }
    for (class, span, owner) in pending {
        if constants.contains(class)
            && !rebound.contains(class)
            && let Some(Some(id)) = methods.get(&(class, &source[span.clone()]))
        {
            call(&mut out, span, owner, *id);
        }
    }
    out
}
