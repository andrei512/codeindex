use super::{
    Analysis, Binding, Evidence, Import, Kind, Language, Lookup, Token, add_use, tok, word,
};
fn import(e: &mut Evidence, scope: usize, module: &str, name: Option<&str>, local: &str) -> usize {
    let index = e.imports.len();
    e.imports.push(Import {
        module: module.to_owned(),
        name: name.map(str::to_owned),
    });
    e.declare(scope, local, Binding::Import(index));
    index
}
fn quoted(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    if b.len() < 2 || !matches!(b[0], b'\'' | b'"') || b.first() != b.last() {
        return None;
    }
    let value = &s[1..s.len() - 1];
    (!value.contains('\\')).then_some(value)
}
// Return the first unconsumed token. Record imports only after recognizing a
// syntactically complete statement with a literal module specifier.
pub(super) fn imports(
    t: &[Token],
    i: usize,
    lang: Language,
    scope: usize,
    a: &mut Analysis,
    e: &mut Evidence,
) -> Option<usize> {
    let mut p = i + 1;
    let mut names = Vec::new();
    let module;
    if lang == Language::Js {
        if t.get(p)?.kind == Kind::String {
            return Some(p + 1);
        }
        if tok(t, p) == "type" {
            p += 1;
        }
        if word(t.get(p)?) {
            names.push((Some("default"), p, p));
            p += 1;
            if tok(t, p) == "," {
                p += 1;
            }
        }
        if tok(t, p) == "*" && tok(t, p + 1) == "as" && word(t.get(p + 2)?) {
            names.push((None, p + 2, p + 2));
            p += 3;
        }
        if tok(t, p) == "{" {
            p += 1;
            while tok(t, p) != "}" {
                if tok(t, p) == "type" && tok(t, p + 1) != "as" {
                    p += 1;
                }
                if !word(t.get(p)?) {
                    return None;
                }
                let original = p;
                p += 1;
                let local = if tok(t, p) == "as" && word(t.get(p + 1)?) {
                    p += 2;
                    p - 1
                } else {
                    original
                };
                names.push((Some(tok(t, original)), original, local));
                if tok(t, p) == "," {
                    p += 1;
                } else if tok(t, p) != "}" {
                    return None;
                }
            }
            p += 1;
        }
        if tok(t, p) != "from" {
            return None;
        }
        module = quoted(tok(t, p + 1))?.to_owned();
        p += 2;
    } else if tok(t, i) == "from" {
        let start = p;
        while t.get(p)?.line == t[i].line
            && tok(t, p) != "import"
            && (word(&t[p]) || tok(t, p) == ".")
        {
            p += 1;
        }
        if p == start || tok(t, p) != "import" {
            return None;
        }
        module = t[start..p].iter().map(|tok| tok.text).collect();
        p += 1;
        if tok(t, p) == "*" {
            let id = e.imports.len();
            e.imports.push(Import { module, name: None });
            e.wildcards.push((scope, id));
            return Some(p + 1);
        }
        let grouped = tok(t, p) == "(";
        if grouped {
            p += 1;
        }
        loop {
            if !word(t.get(p)?) {
                return None;
            }
            let original = p;
            p += 1;
            let local = if tok(t, p) == "as" && word(t.get(p + 1)?) {
                p += 2;
                p - 1
            } else {
                original
            };
            names.push((Some(tok(t, original)), original, local));
            if tok(t, p) != "," {
                break;
            }
            p += 1;
            if grouped && tok(t, p) == ")" {
                break;
            }
        }
        if grouped {
            if tok(t, p) != ")" {
                return None;
            }
            p += 1;
        }
    } else {
        loop {
            let start = p;
            if !word(t.get(p)?) {
                return None;
            }
            p += 1;
            while tok(t, p) == "." && t.get(p + 1).is_some_and(word) {
                p += 2;
            }
            let spec: String = t[start..p].iter().map(|tok| tok.text).collect();
            if tok(t, p) == "as" && t.get(p + 1).is_some_and(word) {
                import(e, scope, &spec, None, tok(t, p + 1));
                p += 2;
            } else if p == start + 1 {
                import(e, scope, &spec, None, tok(t, start));
            } else {
                let id = e.imports.len();
                e.imports.push(Import {
                    module: spec.clone(),
                    name: None,
                });
                e.packages.insert((scope, spec), id);
                e.scopes[scope]
                    .bindings
                    .entry(tok(t, start).to_owned())
                    .or_insert(Binding::Package);
            }
            if tok(t, p) != "," {
                break;
            }
            p += 1;
        }
        return Some(p);
    }
    for (name, original, local) in names {
        let n = import(e, scope, &module, name, tok(t, local));
        if lang == Language::Python && scope == 0 {
            e.forward
                .insert(tok(t, local).to_owned(), super::Forward::Import(n));
        }
        if name.is_some() {
            add_use(a, e, &t[original], scope, false, Lookup::Imported(n));
            if local != original {
                add_use(a, e, &t[local], scope, false, Lookup::Imported(n));
            }
        }
    }
    Some(p)
}

pub(super) fn exports(
    t: &[Token],
    i: usize,
    scope: usize,
    a: &mut Analysis,
    e: &mut Evidence,
) -> Option<usize> {
    if scope != 0 {
        return None;
    }
    let mut p = i + 1;
    if tok(t, p) == "default"
        && t.get(p + 1).is_some_and(word)
        && !super::keyword(Language::Js, tok(t, p + 1))
        && (tok(t, p + 2) == ";" || p + 2 == t.len() || t[p + 1].line < t[p + 2].line)
    {
        let local = p + 1;
        e.forward.insert(
            "default".into(),
            super::Forward::Local(tok(t, local).into()),
        );
        add_use(
            a,
            e,
            &t[local],
            scope,
            false,
            Lookup::Name {
                scope,
                receiver: None,
            },
        );
        return Some(local + 1);
    }
    if tok(t, p) == "type" {
        p += 1;
    }
    if tok(t, p) == "*"
        && tok(t, p + 1) == "as"
        && t.get(p + 2).is_some_and(word)
        && tok(t, p + 3) == "from"
    {
        let module = quoted(tok(t, p + 4))?;
        let n = e.imports.len();
        e.imports.push(Import {
            module: module.into(),
            name: None,
        });
        e.forward
            .insert(tok(t, p + 2).into(), super::Forward::Namespace(n));
        return Some(p + 5);
    }
    if tok(t, p) == "*" && tok(t, p + 1) == "from" {
        let module = quoted(tok(t, p + 2))?;
        e.stars.push(module.to_owned());
        return Some(p + 3);
    }
    if tok(t, p) != "{" {
        return None;
    }
    p += 1;
    let mut items = Vec::new();
    while tok(t, p) != "}" {
        if tok(t, p) == "type" && tok(t, p + 1) != "as" {
            p += 1;
        }
        if !word(t.get(p)?) {
            return None;
        }
        let original = p;
        p += 1;
        let exported = if tok(t, p) == "as" && word(t.get(p + 1)?) {
            p += 2;
            p - 1
        } else {
            original
        };
        items.push((original, exported));
        if tok(t, p) == "," {
            p += 1;
        } else if tok(t, p) != "}" {
            return None;
        }
    }
    p += 1;
    let module = if tok(t, p) == "from" {
        let m = quoted(tok(t, p + 1))?;
        p += 2;
        Some(m)
    } else {
        None
    };
    for (original, public) in items {
        let public = tok(t, public);
        let name = tok(t, original);
        let forward = if let Some(module) = module {
            let imp = e.imports.len();
            e.imports.push(Import {
                module: module.to_owned(),
                name: Some(name.to_owned()),
            });
            add_use(a, e, &t[original], scope, false, Lookup::Imported(imp));
            super::Forward::Import(imp)
        } else {
            add_use(
                a,
                e,
                &t[original],
                scope,
                false,
                Lookup::Name {
                    scope,
                    receiver: None,
                },
            );
            super::Forward::Local(name.to_owned())
        };
        e.forward.insert(public.to_owned(), forward);
    }
    Some(p)
}
