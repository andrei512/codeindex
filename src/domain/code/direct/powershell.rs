//! Literal command invocations and explicitly imported local modules.
use super::super::{
    Analysis, Call, Declaration, ForwardNames, Site, Unit,
    lexer::{Kind, Token},
};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::Path,
};

#[derive(Default)]
pub(in crate::domain::code) struct Evidence {
    imports: Vec<Range<usize>>,
    exports: HashMap<String, usize>,
    qualified: Vec<(usize, Range<usize>)>,
}
crate::domain::code::cache::record!(Evidence {
    imports,
    qualified,
    exports
});

pub(in crate::domain::code) fn analyze(text: &str, tokens: &[Token]) -> Analysis {
    let mut analysis = Analysis::default();
    let mut evidence = Evidence::default();
    let mut declarations = HashMap::new();
    let mut export_list: Option<HashSet<String>> = None;
    let mut module_functions = Vec::new();
    let mut local_calls = Vec::new();
    let mut scopes = Vec::new();
    let mut owner = None;
    let mut pending_owner = None;
    let mut i = 0;
    while let Some(token) = tokens.get(i) {
        let prev = i.checked_sub(1).map(|j| &tokens[j]);
        let command_start = prev.is_none_or(|p| {
            p.line < token.line
                || matches!(p.text, ";" | "|" | "&" | "{" | "(" | "=")
                || p.text.eq_ignore_ascii_case("return")
        });
        i += 1;
        match token.text {
            "{" => {
                scopes.push(owner);
                owner = pending_owner.take().or(owner);
                continue;
            }
            "}" => {
                owner = scopes.pop().flatten();
                continue;
            }
            _ => {}
        }
        if token.kind != Kind::Word || !command_start || token.text.starts_with('$') {
            continue;
        }
        if token.text.eq_ignore_ascii_case("function") {
            if let Some(name) = tokens.get(i).filter(|n| n.kind == Kind::Word) {
                let id = analysis.declarations.len();
                analysis.declarations.push(Declaration {
                    span: name.span.clone(),
                    kind: "function_name",
                });
                declarations
                    .entry((owner, name.text.to_ascii_lowercase()))
                    .and_modify(|d| *d = None)
                    .or_insert(Some(id));
                if scopes.is_empty() {
                    module_functions.push(id);
                }
                pending_owner = Some(id);
                i += 1;
            }
            continue;
        }
        if token.text.eq_ignore_ascii_case("Export-ModuleMember") {
            let allowed = export_list.get_or_insert_with(HashSet::new);
            if scopes.is_empty() {
                let end = tokens[i..]
                    .iter()
                    .position(|n| n.line != token.line || n.text == ";")
                    .map_or(tokens.len(), |n| i + n);
                let args = &tokens[i..end];
                let start = if args.first().is_some_and(|a| a.text == "-") {
                    if !args
                        .get(1)
                        .is_some_and(|a| a.text.eq_ignore_ascii_case("Function"))
                    {
                        continue;
                    }
                    2
                } else {
                    0
                };
                let functions = &args[start..];
                if functions
                    .iter()
                    .all(|n| n.text == "," || literal_name(n.text).is_some())
                {
                    for n in functions {
                        if let Some(name) = literal_name(n.text) {
                            allowed.insert(name.to_ascii_lowercase());
                        }
                    }
                }
            }
            continue;
        }
        if scopes.is_empty() && token.text.eq_ignore_ascii_case("Import-Module") {
            if let Some(arg) = tokens.get(i).filter(|n| n.line == token.line) {
                let start = arg.span.start;
                let mut end = arg.span.end;
                i += 1;
                while let Some(next) = tokens
                    .get(i)
                    .filter(|n| n.span.start == end && n.text != ";")
                {
                    end = next.span.end;
                    i += 1;
                }
                let path = text[start..end].trim_matches(['\'', '"']);
                if !path.contains('$') && path.to_ascii_lowercase().ends_with(".psm1") {
                    let offset = path.as_ptr() as usize - text.as_ptr() as usize;
                    evidence.imports.push(offset..offset + path.len());
                }
            }
            continue;
        }
        let qualified = tokens
            .get(i)
            .is_some_and(|n| n.text == "\\" && n.span.start == token.span.end)
            && tokens
                .get(i + 1)
                .is_some_and(|n| n.kind == Kind::Word && n.span.start == tokens[i].span.end);
        let command = if qualified {
            i += 2;
            &tokens[i - 1]
        } else {
            token
        };
        let call = analysis.calls.len();
        analysis.calls.push(Call {
            span: command.span.clone(),
            owner,
            is_call: true,
        });
        if qualified {
            evidence.qualified.push((call, token.span.clone()));
        } else {
            local_calls.push(call);
        }
    }
    for local in local_calls {
        let c = &analysis.calls[local];
        let key = text[c.span.clone()].to_ascii_lowercase();
        let target = declarations
            .get(&(c.owner, key.clone()))
            .or_else(|| declarations.get(&(None, key)));
        if let Some(Some(id)) = target {
            analysis.direct.push((local, vec![*id]));
        }
    }
    for id in module_functions {
        let name = text[analysis.declarations[id].span.clone()].to_ascii_lowercase();
        if export_list
            .as_ref()
            .is_none_or(|set| set.contains(&name) || set.contains("*"))
            && declarations.get(&(None, name.clone())) == Some(&Some(id))
        {
            evidence.exports.insert(name, id);
        }
    }
    if !evidence.qualified.is_empty() || !evidence.exports.is_empty() {
        analysis.direct_evidence = Some(Box::new(super::Evidence::Powershell(evidence)));
    }
    analysis
}

pub(in crate::domain::code) fn resolve(
    file: usize,
    units: &[Unit],
    paths: &HashMap<&str, usize>,
    names: &mut ForwardNames,
) {
    let u = &units[file];
    let Some(super::Evidence::Powershell(e)) = u.analysis.direct_evidence.as_deref() else {
        return;
    };
    for (local, qualifier) in &e.qualified {
        let c = &u.analysis.calls[*local];
        let module = &u.text[qualifier.clone()];
        let import = e.imports.iter().rev().find(|s| {
            s.end <= c.span.start
                && Path::new(&u.text[(*s).clone()])
                    .file_stem()
                    .and_then(|p| p.to_str())
                    .is_some_and(|p| p.eq_ignore_ascii_case(module))
        });
        let Some(target_file) =
            import.and_then(|s| super::shell::path_for(u.path, &u.text[s.clone()], paths))
        else {
            continue;
        };
        let target = &units[target_file];
        let Some(super::Evidence::Powershell(module)) = target.analysis.direct_evidence.as_deref()
        else {
            continue;
        };
        if let Some(&id) = module
            .exports
            .get(&u.text[c.span.clone()].to_ascii_lowercase())
        {
            names.bind(
                Site {
                    file,
                    local: *local,
                },
                vec![Site {
                    file: target_file,
                    local: id,
                }],
            );
        }
    }
}

fn literal_name(token: &str) -> Option<&str> {
    let name = token.trim_matches(['\'', '"']);
    (!name.is_empty()
        && (name == "*"
            || name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))))
    .then_some(name)
}
