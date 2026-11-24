use super::{Analysis, Token, before, call, colon_name, declare, word};
use std::collections::{HashMap, HashSet};

pub(super) fn analyze<'a>(source: &'a str, t: &[Token<'a>]) -> Analysis {
    let mut out = Analysis::default();
    let mut blocks = Vec::new();
    let mut owner = None;
    let mut package = "main";
    let mut bodies = HashMap::new();
    let mut package_bodies = HashMap::new();
    let mut lexical = HashMap::new();
    let mut lexical_names = HashSet::new();
    let mut subs = HashMap::new();
    let mut pending = Vec::new();
    for (i, kw) in t.iter().enumerate() {
        match kw.text {
            "{" => {
                blocks.push((owner, package));
                if let Some(&id) = bodies.get(&i) {
                    owner = Some(id);
                }
                if let Some(&pkg) = package_bodies.get(&i) {
                    package = pkg;
                }
                continue;
            }
            "}" => {
                if let Some(frame) = blocks.pop() {
                    (owner, package) = frame;
                }
                continue;
            }
            _ => {}
        }
        let prev = before(t, i);
        if !kw.name() || matches!(prev, ">" | ":" | "$" | "@" | "%") {
            continue;
        }
        if matches!(kw.text, "package" | "sub") && prev != "&" {
            let Some(n) = colon_name(t, i + 1) else {
                continue;
            };
            let full = t[i + 1].at..t[n].span().end;
            if kw.text == "package" {
                let name = &source[full.clone()];
                declare(&mut out, full, "package_declaration");
                if word(t, n + 1) == "{" {
                    package_bodies.insert(n + 1, name);
                } else {
                    package = name;
                }
            } else {
                let kind = if word(t, n + 1) == ";" {
                    "function_declaration"
                } else {
                    "function_definition"
                };
                let id = declare(&mut out, t[n].span(), kind);
                let mut body = n + 1;
                if word(t, body) == "(" {
                    let mut depth = 1usize;
                    body += 1;
                    while depth != 0 && body < t.len() {
                        match t[body].text {
                            "(" => depth += 1,
                            ")" => depth -= 1,
                            _ => {}
                        }
                        body += 1;
                    }
                }
                if word(t, body) == "{" {
                    bodies.insert(body, id);
                    if matches!(prev, "my" | "state") {
                        if !lexical_names.insert(t[n].text) {
                            lexical.remove(t[n].text);
                        } else if blocks.is_empty() && n == i + 1 {
                            lexical.insert(t[n].text, (n, id));
                        }
                    } else {
                        let (pkg, name) = source[full]
                            .rsplit_once("::")
                            .unwrap_or((package, t[n].text));
                        subs.entry((pkg, name))
                            .and_modify(|v| *v = None)
                            .or_insert(Some(id));
                    }
                }
            }
        } else if !matches!(prev, "sub" | "package")
            && let Some(n) = colon_name(t, i)
            && word(t, n + 1) == "("
        {
            let name = t[n].text;
            let (pkg, _) = source[t[i].at..t[n].span().end]
                .rsplit_once("::")
                .unwrap_or((package, name));
            pending.push((n, owner, pkg, i != n));
        }
    }
    for (i, owner, pkg, qualified) in pending {
        let name = t[i].text;
        let target = if !qualified && lexical_names.contains(name) {
            lexical.get(name).and_then(|&(n, id)| (i > n).then_some(id))
        } else {
            subs.get(&(pkg, name)).copied().flatten()
        };
        if let Some(id) = target {
            call(&mut out, t[i].span(), owner, id);
        }
    }
    out
}
