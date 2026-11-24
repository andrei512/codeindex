use super::{Analysis, Call, Declaration, Kind, Token};
use crate::domain::code::{ForwardNames, Site, Unit};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path},
};
#[derive(Default)]
pub(in crate::domain::code) struct Evidence {
    types: HashMap<String, usize>,
    imports: Vec<(String, bool)>,
    lookups: Vec<(std::ops::Range<usize>, String, String)>,
}
fn word<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    t.get(i).map_or("", |x| x.text)
}
fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}.{name}")
    }
}
fn scalar(name: &str) -> bool {
    matches!(
        name,
        "double"
            | "float"
            | "int32"
            | "int64"
            | "uint32"
            | "uint64"
            | "sint32"
            | "sint64"
            | "fixed32"
            | "fixed64"
            | "sfixed32"
            | "sfixed64"
            | "bool"
            | "string"
            | "bytes"
    )
}
pub(super) fn analyze(t: &[Token]) -> Analysis {
    let mut out = Analysis::default();
    let mut ev = Evidence::default();
    let mut scope = String::new();
    let mut pending = None;
    let mut restore = vec![];
    let mut i = 0;
    while i < t.len() {
        let tok = &t[i];
        match tok.text {
            "package" => {
                i += 1;
                while i < t.len() && word(t, i) != ";" {
                    scope.push_str(word(t, i));
                    i += 1
                }
            }
            "import" => {
                let public = word(t, i + 1) == "public";
                let j = i + 1 + usize::from(matches!(word(t, i + 1), "public" | "weak"));
                if t.get(j).is_some_and(|v| v.kind == Kind::String) {
                    ev.imports
                        .push((word(t, j).trim_matches(['\'', '"']).into(), public))
                }
            }
            "message" | "enum" | "service" | "rpc"
                if t.get(i + 1).is_some_and(|v| v.kind == Kind::Word) =>
            {
                i += 1;
                let name = t[i].text;
                let kind = match tok.text {
                    "message" => "message_name",
                    "enum" => "enum_name",
                    "service" => "service_name",
                    _ => "rpc_name",
                };
                let key = join(&scope, name);
                if matches!(tok.text, "message" | "enum") {
                    ev.types.insert(key.clone(), out.declarations.len());
                }
                if tok.text != "rpc" {
                    pending = Some(key)
                }
                out.declarations.push(Declaration {
                    span: t[i].span.clone(),
                    kind,
                });
            }
            "{" => {
                restore.push(scope.len());
                if let Some(s) = pending.take() {
                    scope = s
                }
            }
            "}" => {
                if let Some(n) = restore.pop() {
                    scope.truncate(n)
                }
                pending = None
            }
            ";" => pending = None,
            _ if tok.kind == Kind::Word
                && i.checked_sub(1)
                    .is_none_or(|j| !matches!(word(t, j), "package" | "import")) =>
            {
                let mut last = i;
                while word(t, last + 1) == "."
                    && t.get(last + 2).is_some_and(|s| s.kind == Kind::Word)
                {
                    last += 2
                }
                let field = t.get(last + 1).is_some_and(|v| v.kind == Kind::Word)
                    && word(t, last + 2) == "=";
                let prev = i.checked_sub(1).map_or("", |j| word(t, j));
                let rpc = word(t, last + 1) == ")" && matches!(prev, "(" | "." | "stream");
                let map = word(t, last + 1) == ">" && matches!(prev, "," | ".");
                if (field || rpc || map) && !scalar(tok.text) {
                    let absolute = prev == ".";
                    let start = if absolute { i - 1 } else { i };
                    let key = t[start..=last].iter().map(|v| v.text).collect::<String>();
                    let first = out.calls.len();
                    // The public projection exposes the full type path plus each identifier after its first component.
                    out.calls.push(Call {
                        span: t[start].span.start..t[last].span.end,
                        owner: None,
                        is_call: false,
                    });
                    for k in (i + 2..=last).step_by(2) {
                        out.calls.push(Call {
                            span: t[k].span.clone(),
                            owner: None,
                            is_call: false,
                        })
                    }
                    ev.lookups
                        .push((first..out.calls.len(), scope.clone(), key));
                }
                i = last;
            }
            _ => {}
        }
        i += 1;
    }
    out.direct_evidence = Some(Box::new(super::Evidence::Proto(ev)));
    out
}
fn evidence<'a>(unit: &'a Unit) -> Option<&'a Evidence> {
    match unit.analysis.direct_evidence.as_deref()? {
        super::Evidence::Proto(e) => Some(e),
        _ => None,
    }
}
fn locate_import(path: &str, import: &str, paths: &HashMap<&str, usize>) -> Option<usize> {
    let input = Path::new(import);
    if input
        .components()
        .any(|s| !matches!(s, Component::Normal(_)))
    {
        return None;
    }
    Path::new(path)
        .parent()?
        .ancestors()
        .find_map(|base| paths.get(base.join(input).to_str()?).copied())
}
pub(super) fn resolve(units: &[Unit], paths: &HashMap<&str, usize>, names: &mut ForwardNames) {
    let imports: HashMap<_, _> = units
        .iter()
        .enumerate()
        .filter_map(|(id, u)| {
            let ev = evidence(u)?;
            Some((
                id,
                ev.imports
                    .iter()
                    .filter_map(|(p, public)| Some((locate_import(u.path, p, paths)?, *public)))
                    .collect::<Vec<_>>(),
            ))
        })
        .collect();
    for (&file, imps) in &imports {
        let mut visible = HashSet::from([file]);
        let mut todo: Vec<_> = imps.iter().map(|&(id, _)| id).collect();
        while let Some(id) = todo.pop() {
            if visible.insert(id)
                && let Some(edges) = imports.get(&id)
            {
                todo.extend(edges.iter().filter_map(|&(f, public)| public.then_some(f)))
            }
        }
        let e = evidence(&units[file]).unwrap();
        for (sites, scope, spelling) in &e.lookups {
            let mut prefix = if spelling.starts_with('.') {
                ""
            } else {
                scope.as_str()
            };
            loop {
                let full = join(prefix, spelling.trim_start_matches('.'));
                let targets: Vec<_> = visible
                    .iter()
                    .filter_map(|&f| {
                        Some(Site {
                            file: f,
                            local: *evidence(&units[f])?.types.get(&full)?,
                        })
                    })
                    .collect();
                if !targets.is_empty() {
                    if targets.len() == 1 {
                        for local in sites.clone() {
                            names.bind(Site { file, local }, targets.clone())
                        }
                    }
                    break;
                }
                if prefix.is_empty() {
                    break;
                }
                prefix = prefix.rsplit_once('.').map_or("", |(p, _)| p);
            }
        }
    }
}

crate::domain::code::cache::record!(Evidence { types, imports, lookups });
