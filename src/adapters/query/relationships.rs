use super::{
    Pin,
    json::{array, object, quote},
};
use crate::domain::code::Language;
use crate::{
    adapters::snapshot::manifest::{REL, SYMBOLS},
    application::index::{
        binary::bad, graph::Decl, relationship_lookup::Relations, symbol_lookup::SymbolLookup,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Component, Path},
};
pub(super) fn relative(root: &Path, cwd: &Path, input: &str) -> io::Result<String> {
    let path = cwd.join(input);
    let mut out = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            _ => out.push(c.as_os_str()),
        }
    }
    out.strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("file outside worktree"))
}
fn byte_at(text: &str, start: usize, col: usize) -> io::Result<usize> {
    let line = text
        .get(start..)
        .ok_or_else(bad)?
        .split('\n')
        .next()
        .unwrap();
    let mut offset = 0;
    let mut units = 1;
    for c in line.chars() {
        if units >= col {
            break;
        }
        units += c.len_utf16();
        offset += c.len_utf8();
    }
    if units != col {
        return Err(io::Error::other(
            "position outside line or inside surrogate pair",
        ));
    }
    Ok(start + offset)
}
fn identity(pin: &Pin, d: &Decl) -> String {
    if let Some(label) = &d.label {
        return label.clone();
    }
    let path = pin.files[d.file].path();
    let engine = match Language::for_path(path) {
        Some(Language::Html) => "vscode-html-language-server",
        Some(Language::Css) => "vscode-css-language-server",
        Some(Language::Proto) => "protols",
        Some(Language::C | Language::Cpp) => "clangd",
        Some(Language::Shell) => "bash-language-server",
        Some(Language::Powershell) => "pwsh",
        Some(Language::Swift) => "sourcekit-lsp",
        _ => return format!("native {path} {} {}", d.span.start, d.name),
    };
    let navigation_only = matches!(
        d.kind.as_str(),
        "local_binding" | "parameter" | "shell_assignment" | "field_declaration" | "css_variable"
    );
    if navigation_only {
        format!("{engine} {path} {}", d.span.start)
    } else {
        format!("{engine} {path} {} {}", d.span.start, d.name)
    }
}
pub enum Subject<'a> {
    Name(&'a str),
    Position(&'a str, usize, usize),
}

pub fn query(
    pin: &Pin,
    cwd: &Path,
    op: &str,
    subject: Subject<'_>,
    display: &super::Display,
    outcome: &mut super::Outcome,
) -> io::Result<String> {
    let compact = display.public && !display.json;
    let mut symbols = SymbolLookup::open(pin.reader(SYMBOLS)?, pin.files.len())?;
    let mut decls = BTreeMap::new();
    let (path, file, position) = match subject {
        Subject::Name(name) => {
            let (id, decl) = match symbols.named_target(name) {
                Err(e) if display.public && e.kind() == io::ErrorKind::NotFound => {
                    outcome.code = 1;
                    outcome.diagnostics = format!("cx: unresolved name {name}\n");
                    return Ok(if compact {
                        String::new()
                    } else {
                        response(
                            pin,
                            op,
                            false,
                            "unresolved",
                            "[]".into(),
                            vec![],
                            vec![],
                            false,
                        )
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
                    let symbol = name.rsplit("::").next().unwrap_or(name);
                    let advice = if display.public {
                        format!("cx --symbol {symbol} --column and pass the selected path, line and byte column to cx --reference from the repository root")
                    } else {
                        format!("cx --symbol {symbol} --json and pass the selected path, line and col to cx --cx {op} from the repository root")
                    };
                    return Err(io::Error::new(e.kind(), format!("{e}; use {advice}")));
                }
                result => result?,
            };
            let file = decl.file;
            decls.insert(id, decl);
            (pin.files[file].path().to_owned(), Some(file), None)
        }
        Subject::Position(input, line, col) => {
            let path = relative(&pin.root, cwd, input)?;
            let file = pin.files.binary_search_by(|f| f.path().cmp(&path)).ok();
            (path, file, Some((line, col)))
        }
    };
    let mut profile = Language::for_path(&path);
    if profile == Some(Language::Sql)
        && let Some(file) = file
    {
        symbols.visit_file(file, |_, declaration| {
            if matches!(
                declaration.kind.as_str(),
                "plpgsql_function" | "plpgsql_procedure"
            ) {
                profile = Some(Language::PlPgSql);
            }
        })?;
    }
    let unsupported = op == "callers"
        && matches!(
            profile,
            Some(
                Language::Proto
                    | Language::Html
                    | Language::Css
                    | Language::Sql
                    | Language::Starlark
                    | Language::Procfile
            )
        );
    let indexed = file.is_some_and(|i| pin.files[i].bytes.is_some())
        && profile.is_some()
        && !matches!(profile, Some(Language::Sql | Language::Starlark));
    let context_id = context_key(pin, &path, profile);
    if !indexed || unsupported {
        if display.public {
            outcome.code = 2;
        }
        if compact {
            outcome.diagnostics = if indexed {
                "cx: unsupported operation\n"
            } else {
                "cx: unindexed document\n"
            }
            .into();
            return Ok(String::new());
        }
        let contexts = if indexed {
            array([object(&[
                ("context", quote(&context_id)),
                ("coverage", quote("unsupported")),
                ("reason", quote("unsupported_operation")),
                ("bindings", "[]".into()),
            ])])
        } else {
            "[]".into()
        };
        return Ok(response(
            pin,
            op,
            indexed,
            if unsupported {
                "unsupported"
            } else {
                "unindexed"
            },
            contexts,
            Vec::new(),
            Vec::new(),
            false,
        ));
    }
    let file = file.ok_or_else(bad)?;
    let mut graph = Relations::open(pin.reader(REL)?, pin.files.len(), symbols.count())?;
    let mut targets = BTreeSet::new();
    let mut bindings = BTreeMap::<&str, BTreeSet<usize>>::new();
    if let Some((line, col)) = position {
        let start = pin.files[file]
            .line_start(line)
            .ok_or_else(|| io::Error::other("line outside document"))?;
        let entry = &pin.files[file];
        let end = entry
            .line_ends
            .get(line - 1)
            .map(|&n| n as u64)
            .unwrap_or_else(|| entry.bytes.as_ref().map_or(0, |r| r.end - r.start));
        let text = pin.source_slice(file, start as u64..end)?;
        let at = start
            + if display.public {
                let offset = col - 1;
                let body = text.trim_end_matches(['\r', '\n']);
                if offset >= body.len() || !body.is_char_boundary(offset) {
                    return Err(io::Error::other("invalid byte column"));
                }
                offset
            } else {
                byte_at(&text, 0, col)?
            };
        let mut shortest = usize::MAX;
        let mut select = |span: &std::ops::Range<usize>, ids: &[usize], role: &'static str| {
            if span.contains(&at) && span.len() <= shortest {
                if span.len() < shortest {
                    targets.clear();
                    bindings.clear();
                    shortest = span.len();
                }
                targets.extend(ids.iter().copied());
                bindings
                    .entry(role)
                    .or_default()
                    .extend(ids.iter().copied());
            }
        };
        symbols.visit_file(file, |id, d| {
            if d.kind != "source_file" {
                select(&d.span, &[id], "definition");
            }
        })?;
        graph.visit_file(file, |u| {
            select(
                &u.span,
                &u.targets,
                if u.is_call {
                    "call_target"
                } else {
                    "reference"
                },
            )
        })?;
        for &id in &targets {
            decls.insert(id, symbols.semantic_declaration(id as u64)?);
        }
    } else {
        targets.extend(decls.keys().copied());
        bindings.insert("definition", targets.clone());
    }
    // Keep only the first 50 distinct public rows. A later candidate can
    // displace the largest key; once truncated, the flag can never clear.
    let mut ordered =
        BTreeMap::<(usize, usize, Option<String>, Option<String>), BTreeSet<String>>::new();
    let mut truncated = false;
    let mut save = |key, target: &str| {
        ordered.entry(key).or_default().insert(target.to_owned());
        if ordered.len() > 50 {
            ordered.pop_last();
            truncated = true;
        }
    };
    for &id in &targets {
        let d = &decls[&id];
        let target = identity(pin, d);
        if op == "definition" {
            save((d.file, d.span.start, Some(target.clone()), None), &target);
            continue;
        }
        for c in graph.incoming(id)? {
            let u = graph.site(c)?;
            if op == "callers" && !u.is_call {
                continue;
            }
            let (symbol, caller) = if op == "callers" {
                if let Some(owner) = u.owner {
                    if let std::collections::btree_map::Entry::Vacant(e) = decls.entry(owner) {
                        e.insert(symbols.semantic_declaration(owner as u64)?);
                    }
                    let d = &decls[&owner];
                    (Some(identity(pin, d)), Some(d.name.clone()))
                } else {
                    (None, None)
                }
            } else {
                (Some(target.clone()), None)
            };
            save((u.file, u.span.start, symbol, caller), &target);
        }
    }
    let mut hits = Vec::new();
    if display.public {
        outcome.code = u8::from(ordered.is_empty());
    }
    if compact {
        if truncated {
            outcome.diagnostics.push_str("cx: results truncated\n");
        }
        if targets.is_empty() {
            outcome.diagnostics.push_str("cx: unresolved position\n");
        }
        outcome
            .diagnostics
            .push_str("cx: partial reference coverage\n");
        let sites = ordered.into_keys().map(|(f, b, _, _)| (f, b)).collect();
        return super::display::render(pin, cwd, sites, display, false);
    }
    let mut evidence = Vec::new();
    let mut previous = None;
    for ((f, byte, symbol, caller), ids) in ordered {
        for target in ids {
            evidence.push(object(&[
                ("hit", hits.len().to_string()),
                ("context", quote(&context_id)),
                (
                    "role",
                    quote(match op {
                        "callers" => "call_target",
                        "definition" => "definition",
                        _ => "reference",
                    }),
                ),
                ("certainty", quote("candidates")),
                ("target", quote(&target)),
            ]));
        }
        let (line, _) = pin.files[f].line_column(byte);
        let (from, base) = match previous {
            Some((file, prev_line, prev_byte, prev_col)) if file == f && prev_line == line => {
                (prev_byte, prev_col)
            }
            _ => (pin.files[f].line_start(line).ok_or_else(bad)?, 1usize),
        };
        let col = if from == byte {
            base
        } else {
            base + pin
                .source_slice(f, from as u64..byte as u64)?
                .encode_utf16()
                .count()
        };
        previous = Some((f, line, byte, col));
        let mut fields = vec![
            ("path", quote(pin.files[f].path())),
            ("line", line.to_string()),
            ("col", col.to_string()),
            (
                "symbol",
                symbol.as_deref().map_or_else(|| "null".into(), quote),
            ),
        ];
        if op == "callers" {
            fields.push((
                "caller",
                caller.as_deref().map_or_else(|| "null".into(), quote),
            ));
        }
        hits.push(object(&fields));
    }
    let status = if targets.is_empty() {
        "unresolved"
    } else {
        "resolved"
    };
    let bindings = array(bindings.into_iter().map(|(role, ids)| {
        let labels: BTreeSet<_> = ids
            .into_iter()
            .map(|id| identity(pin, &decls[&id]))
            .collect();
        object(&[
            ("role", quote(role)),
            (
                "certainty",
                quote(if labels.is_empty() {
                    "unresolved"
                } else {
                    "candidates"
                }),
            ),
            ("targets", array(labels.iter().map(|s| quote(s)))),
        ])
    }));
    let context = object(&[
        ("context", quote(&context_id)),
        ("coverage", quote("partial")),
        ("reason", quote("unverified_context")),
        ("bindings", bindings),
    ]);
    Ok(response(
        pin,
        op,
        true,
        status,
        array([context]),
        hits,
        evidence,
        truncated,
    ))
}
fn context_key(pin: &Pin, path: &str, profile: Option<Language>) -> String {
    use std::fmt::Write;
    let (pack, manifest) = match profile {
        Some(Language::Rust) => ("rust", Some("Cargo.toml")),
        Some(Language::Js) => (
            match Path::new(path).extension().and_then(|e| e.to_str()) {
                Some("tsx") => "typescriptreact",
                Some("jsx") => "javascriptreact",
                Some("ts" | "cts" | "mts") => "typescript",
                _ => "javascript",
            },
            Some("tsconfig.json"),
        ),
        Some(Language::Python) => ("python", None),
        Some(Language::Csharp) => ("csharp", None),
        Some(Language::Cpp) => ("cpp", None),
        Some(Language::C) => ("c", None),
        Some(Language::Swift) => ("swift", None),
        Some(Language::Proto) => ("proto", None),
        Some(Language::Css) => ("css", None),
        Some(Language::Shell) => ("shellscript", None),
        Some(Language::Powershell) => ("powershell", None),
        Some(Language::Html) => ("html", None),
        Some(Language::Sql) => ("sql", None),
        Some(Language::Starlark) => ("starlark", None),
        Some(lang) => (lang.extended_name().unwrap_or("unknown"), None),
        None => ("unknown", None),
    };
    let entry = manifest
        .and_then(|name| {
            Path::new(path).ancestors().skip(1).find_map(|dir| {
                let key = dir.join(name);
                let i = pin
                    .files
                    .binary_search_by(|f| f.path().cmp(key.to_str().unwrap()))
                    .ok()?;
                Some(pin.files[i].path())
            })
        })
        .unwrap_or(".");
    let mut id = String::from("ctx1/l4/");
    for value in [pack, entry, ""] {
        write!(id, "s{}/", value.len()).unwrap();
        for byte in value.bytes() {
            write!(id, "{byte:02x}").unwrap();
        }
    }
    id.push_str("m0/");
    id
}
#[allow(clippy::too_many_arguments)]
fn response(
    pin: &Pin,
    op: &str,
    indexed: bool,
    status: &str,
    contexts: String,
    hits: Vec<String>,
    evidence: Vec<String>,
    truncated: bool,
) -> String {
    object(&[
        ("root", quote(&pin.root.to_string_lossy())),
        ("mode", quote(op)),
        ("generation", pin.generation.to_string()),
        ("reused", pin.reused.to_string()),
        ("indexed_document", indexed.to_string()),
        (
            "engine",
            if indexed {
                quote("codeindex")
            } else {
                "null".into()
            },
        ),
        ("operation_status", quote(status)),
        ("relationship_status", quote("partial")),
        ("position_encoding", quote("one-based UTF-16")),
        ("contexts", contexts),
        ("result", array(hits)),
        ("evidence", array(evidence)),
        ("truncated", truncated.to_string()),
    ])
}
