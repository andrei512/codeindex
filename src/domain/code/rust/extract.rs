use super::*;

pub(in crate::domain::code) fn analyze(t: &[Token], text_len: usize) -> Analysis {
    analyze_paired(t, text_len, &matches(t))
}
pub(in crate::domain::code) fn analyze_paired(
    t: &[Token],
    text_len: usize,
    pairs: &[usize],
) -> Analysis {
    let mut a = Analysis::default();
    let mut e = Evidence {
        conditional_crate: false,
        conditions: Vec::new(),
        scopes: vec![Scope::default()],
        uses: Vec::new(),
        members: Default::default(),
        direct: Default::default(),
    };
    let mut scope = 0;
    let mut future = HashMap::new();
    let mut excluded = vec![false; t.len()];
    let mut ends = HashMap::new();
    let mut starts = HashMap::new();
    let mut match_blocks = HashSet::new();
    let mut generic_scopes = Vec::new();
    let mut delimiters = Vec::new();
    let mut use_at = vec![None; t.len()];
    let mut rhs_uses = Vec::new();
    let mut i = 0;
    while i < t.len() {
        generics::leave(&mut generic_scopes, i);
        if let Some(parent) = ends.remove(&i) {
            scope = parent;
        }
        if let Some(arm) = starts.remove(&i) {
            scope = arm;
        }
        let s = t[i].text;
        while delimiters.last().is_some_and(|&open| pairs[open] <= i) {
            delimiters.pop();
        }
        if matches!(s, "{" | "(" | "[") && pairs[i] != usize::MAX {
            delimiters.push(i);
        }
        if scope == 0
            && s == "#"
            && word(t, i + 1) == "!"
            && word(t, i + 2) == "["
            && word(t, i + 3) == "cfg"
        {
            e.conditional_crate = true;
        }
        let prev = if i == 0 { "" } else { t[i - 1].text };
        if s == "#"
            && word(t, i + 1) == "["
            && let Some(c) = config::condition(t, &pairs, i)
        {
            e.conditions.push(c);
        }
        if (s == "#" && word(t, i + 1) == "[")
            || (s == "!"
                && (ident(prev) || prev == "#")
                && !matches!(
                    prev,
                    "format"
                        | "matches"
                        | "format_args"
                        | "write"
                        | "writeln"
                        | "println"
                        | "print"
                        | "eprintln"
                        | "assert"
                        | "assert_eq"
                        | "assert_ne"
                        | "debug_assert"
                        | "debug_assert_eq"
                        | "vec"
                        | "dbg"
                        | "panic"
                        | "warn"
                        | "debug"
                        | "info"
                        | "error"
                        | "trace"
                        | "anyhow"
                        | "bail"
                        | "ensure"
                )
                && matches!(word(t, i + 1), "(" | "[" | "{"))
        {
            i = pairs[i + 1].checked_add(1).unwrap_or(t.len());
            continue;
        }
        if s == "macro_rules" && word(t, i + 1) == "!" && ident(word(t, i + 2)) {
            declaration(
                &mut a,
                &mut e,
                t,
                i + 2,
                scope,
                "macro_definition",
                0,
                Namespace::Type,
            );
            i = pairs
                .get(i + 3)
                .and_then(|v| v.checked_add(1))
                .unwrap_or(t.len());
            continue;
        }
        if s == "use" {
            let end = until(t, &pairs, i + 1, &[";", "}"]);
            imports(&t[i + 1..end], scope, &mut e, &mut a);
            i = end;
            if word(t, i) == ";" {
                i += 1;
            }
            continue;
        }
        // Return-position and argument-position "impl Trait" introduce no
        // item scope and must preserve the enclosing function and parameters.
        let impl_item = s == "impl"
            && matches!(
                prev,
                "" | ";" | "{" | "}" | "]" | "unsafe" | "default" | "const"
            );
        if impl_item {
            generics::enter(
                t,
                &pairs,
                i,
                i + 1,
                &mut a,
                &mut excluded,
                &mut generic_scopes,
                &mut e.members,
            );
        }
        if impl_item || s == "trait" {
            let open = until(t, &pairs, i + 1, &["{", ";", "}"]);
            if word(t, open) == "{" {
                let child = e.scopes.len();
                e.scopes.push(Scope {
                    parent: Some(scope),
                    owner: e.scopes[scope].owner,
                    module: e.scopes[scope].module,
                    associated: true,
                    ..Default::default()
                });
                if s == "impl" {
                    let (ty, inherent) = members::impl_path(t, i + 1, open);
                    if open == i + 2 && ident(word(t, i + 1)) {
                        e.members.simple_impls.insert(child, None);
                    }
                    if open == i + 4
                        && ident(word(t, i + 1))
                        && word(t, i + 2) == "for"
                        && ident(word(t, i + 3))
                    {
                        e.members
                            .simple_impls
                            .insert(child, Some(word(t, i + 1).into()));
                    }
                    if !ty.is_empty() {
                        e.members.impls.insert(child, (ty, inherent));
                    }
                }
                future.insert(open, child);
            }
        }
        if s == "match" {
            let open = until(t, &pairs, i + 1, &["{", ";", "}"]);
            if word(t, open) == "{" {
                match_blocks.insert(open);
            }
        }
        if s == "{" {
            if let Some(child) = future.remove(&i) {
                scope = child;
            } else {
                let child = Scope {
                    literal: members::before(t, i),
                    parent: Some(scope),
                    owner: e.scopes[scope].owner,
                    module: e.scopes[scope].module,
                    ..Default::default()
                };
                scope = e.scopes.len();
                e.scopes.push(child);
            }
            if match_blocks.remove(&i) {
                patterns::arms(
                    t,
                    &pairs,
                    i,
                    scope,
                    &mut a,
                    &mut e,
                    &mut excluded,
                    &mut starts,
                    &mut ends,
                );
            }
            i += 1;
            continue;
        }
        if s == "}" {
            scope = e.scopes[scope].parent.unwrap_or(0);
            i += 1;
            continue;
        }
        let item = match s {
            "fn" => Some(("function_item", Namespace::Value)),
            "struct" => Some(("struct_item", Namespace::Either)),
            "enum" => Some(("enum_item", Namespace::Type)),
            "trait" => Some(("trait_item", Namespace::Type)),
            "mod" => Some(("mod_item", Namespace::Type)),
            "type" => Some(("type_item", Namespace::Type)),
            "const" => Some(("const_item", Namespace::Value)),
            "static" => Some(("static_item", Namespace::Value)),
            _ => None,
        };
        let id_token = i + 1 + usize::from(s == "static" && word(t, i + 1) == "mut");
        if let Some((kind, ns)) = item.filter(|_| {
            !excluded[i]
                && (ident(word(t, id_token)) || s == "const" && word(t, id_token) == "_")
                && !matches!(prev, "'" | "*")
        }) {
            let kind = if s == "type"
                && e.scopes[scope].associated
                && !has_type_assignment(t, id_token + 1)
            {
                "associated_type"
            } else {
                kind
            };
            let id = declaration(&mut a, &mut e, t, id_token, scope, kind, 0, ns);
            excluded[id_token] = true;
            if matches!(s, "struct" | "enum" | "trait") && word(t, id_token + 1) != "<" {
                e.members.plain_types.insert(id);
            }
            if s == "trait" {
                let open = until(t, &pairs, id_token + 1, &["{", ";", "}"]);
                if let Some(&child) = future.get(&open) {
                    e.members.trait_scopes.insert(child, id);
                }
            }
            if matches!(s, "fn" | "struct" | "enum" | "type" | "trait") {
                generics::enter(
                    t,
                    &pairs,
                    i,
                    id_token + 1,
                    &mut a,
                    &mut excluded,
                    &mut generic_scopes,
                    &mut e.members,
                );
            }
            if matches!(s, "const" | "static") && word(t, id_token + 1) == ":" {
                e.members.note_arguments(id, scope, t, id_token + 2);
                let ty = members::path(t, id_token + 2);
                if !ty.is_empty() {
                    e.members.types.insert(id, (scope, ty));
                }
            }
            if s == "type" {
                let eq = until(t, &pairs, id_token + 1, &["=", ";", "}"]);
                if word(t, eq) == "=" {
                    e.members.note_arguments(id, scope, t, eq + 1);
                    let ty = members::path(t, eq + 1);
                    if !ty.is_empty() {
                        e.members.types.insert(id, (scope, ty));
                    }
                }
            }
            if matches!(s, "struct" | "enum") {
                let open = until(t, &pairs, id_token + 1, &["{", ";", "}"]);
                if word(t, open) == "{" && pairs[open] != usize::MAX {
                    future
                        .entry(open)
                        .or_insert_with(|| new_scope(&mut e, scope));
                    members::fields(
                        &mut a,
                        &mut e,
                        t,
                        &pairs,
                        &mut excluded,
                        scope,
                        id,
                        open,
                        s == "enum",
                    );
                }
            }
            if s == "mod" && word(t, id_token + 1) == ";" {
                e.scopes[scope].module(
                    t[id_token].text,
                    Module::File(path_attribute(t, &pairs, i)),
                    t[id_token].span.start,
                );
            }
            if s == "mod" && word(t, id_token + 1) == "{" {
                let child = e.scopes.len();
                e.scopes.push(Scope {
                    parent: Some(scope),
                    module: child,
                    ..Default::default()
                });
                e.scopes[scope].module(
                    t[id_token].text,
                    Module::Inline(child),
                    t[id_token].span.start,
                );
                future.insert(id_token + 1, child);
            }
            if s == "fn" {
                let open = until(t, &pairs, id_token + 1, &["(", "{", ";", "}"]);
                if word(t, open) == "(" && pairs[open] != usize::MAX {
                    let close = pairs[open];
                    let body = until(t, &pairs, close + 1, &["{", ";", "}"]);
                    if word(t, body) == "{" {
                        let child = e.scopes.len();
                        e.scopes.push(Scope {
                            parent: Some(scope),
                            module: e.scopes[scope].module,
                            owner: Some(id),
                            ..Default::default()
                        });
                        future.insert(body, child);
                        let mut param = open + 1;
                        if word(t, close + 1) == "-" && word(t, close + 2) == ">" {
                            e.members.note_arguments(id, child, t, close + 3);
                            let ty = members::path(t, close + 3);
                            if !ty.is_empty() {
                                e.members.types.insert(id, (child, ty));
                            }
                        }
                        while param < close {
                            while matches!(word(t, param), "&" | "mut" | "ref") {
                                param += 1;
                            }
                            if word(t, param) == "self"
                                || ident(word(t, param)) && word(t, param + 1) == ":"
                            {
                                let decl = declaration(
                                    &mut a,
                                    &mut e,
                                    t,
                                    param,
                                    child,
                                    "parameter",
                                    0,
                                    Namespace::Value,
                                );
                                e.members.note_arguments(decl, child, t, param + 2);
                                let ty = if word(t, param) == "self" {
                                    vec!["Self".into()]
                                } else {
                                    members::path(t, param + 2)
                                };
                                if !ty.is_empty() {
                                    e.members.types.insert(decl, (child, ty));
                                }
                                excluded[param] = true;
                            }
                            param = until(t, &pairs, param, &[",", ")"]) + 1;
                        }
                    } else if word(t, body) == ";" {
                        a.declarations[id].kind = "function_signature_item";
                    }
                }
            }
        }
        if s == "let" || s == "for" {
            let stop = until(t, &pairs, i + 1, &["=", ";", ":", "in", "}"]);
            let conditional =
                matches!(prev, "while" | "if") || prev == "&" && i > 1 && word(t, i - 2) == "&";
            if s == "for" && word(t, stop) == "in"
                || s == "let" && conditional && word(t, stop) == "="
            {
                let open = until(t, &pairs, stop + 1, &["{", ";", "}"]);
                if word(t, open) == "{" {
                    let child = *future
                        .entry(open)
                        .or_insert_with(|| new_scope(&mut e, scope));
                    let mut guard = open;
                    if conditional {
                        let mut j = stop + 1;
                        while j < open {
                            if word(t, j) == "&" && word(t, j + 1) == "&" {
                                guard = j;
                                starts.insert(j, child);
                                break;
                            }
                            if matches!(word(t, j), "(" | "[" | "{") && pairs[j] != usize::MAX {
                                j = pairs[j] + 1;
                            } else {
                                j += 1;
                            }
                        }
                    }
                    let first_decl = a.declarations.len();
                    bind_pattern(
                        &mut a,
                        &mut e,
                        t,
                        &mut excluded,
                        child,
                        i + 1..stop,
                        "local_binding",
                        t[guard].span.start,
                    );
                    if a.declarations.len() == first_decl + 1 {
                        let projection = if s == "for" {
                            Some(Projection::Element)
                        } else if matches!(word(t, i + 1), "Some" | "Ok") {
                            Some(Projection::Unwrap)
                        } else {
                            None
                        };
                        if let Some(projection) = projection {
                            let end = members::strip_iteration(t, open, projection);
                            if let Some(at) = members::simple_expression(t, &pairs, stop + 1, end) {
                                rhs_uses.push((first_decl, at, projection));
                            }
                        }
                    }
                }
            } else if s == "let" && matches!(word(t, stop), "=" | ";" | ":") {
                let end = until(t, &pairs, stop, &[";", "}"]);
                if word(t, end) == ";" {
                    let first_decl = a.declarations.len();
                    bind_pattern(
                        &mut a,
                        &mut e,
                        t,
                        &mut excluded,
                        scope,
                        i + 1..stop,
                        "local_binding",
                        t[end].span.end,
                    );
                    if word(t, i + 1) == "("
                        && word(t, stop) == "="
                        && let Some(rhs) = members::simple_expression(t, &pairs, stop + 1, end)
                    {
                        let mut part = i + 2;
                        let mut index = 0;
                        while part < pairs[i + 1] && part < stop {
                            let end = until(t, &pairs, part, &[",", ")"]).min(stop);
                            let name_at = part + usize::from(word(t, part) == "mut");
                            if end == name_at + 1
                                && let Some(id) = a.declarations[first_decl..]
                                    .iter()
                                    .position(|d| d.span == t[name_at].span)
                            {
                                rhs_uses.push((first_decl + id, rhs, Projection::Tuple(index)));
                            }
                            index += 1;
                            part = end + 1;
                        }
                    }
                    if a.declarations.len() == first_decl + 1 {
                        if word(t, stop) == ":" {
                            e.members.note_arguments(first_decl, scope, t, stop + 1);
                        }
                        if word(t, stop) == "=" && word(t, i + 1) != "(" {
                            let (expr_end, projection) = members::result_suffix(t, end);
                            if let Some(at) =
                                members::simple_expression(t, &pairs, stop + 1, expr_end)
                            {
                                rhs_uses.push((first_decl, at, projection));
                            }
                        }
                        let ty = members::path(t, stop + 1);
                        if !ty.is_empty()
                            && (word(t, stop) == ":"
                                || word(t, stop + 1 + (ty.len() - 1) * 3 + 1) == "{")
                        {
                            e.members.types.insert(first_decl, (scope, ty));
                        }
                    }
                }
            }
        }
        if s == "|"
            && matches!(
                prev,
                "(" | "," | "=" | "move" | "async" | ">" | "return" | "["
            )
        {
            let stop = until(t, &pairs, i + 1, &["|", ";", ")", "}"]);
            if word(t, stop) == "|" {
                let child = new_scope(&mut e, scope);
                let mut first = i + 1;
                while first < stop {
                    let end = until(t, &pairs, first, &[",", "|", ":"]).min(stop);
                    let decl = a.declarations.len();
                    bind_pattern(
                        &mut a,
                        &mut e,
                        t,
                        &mut excluded,
                        child,
                        first..end,
                        "parameter",
                        0,
                    );
                    if a.declarations.len() == decl + 1 && end == first + 1 {
                        if word(t, end) == ":" {
                            let ty = members::path(t, end + 1);
                            if !ty.is_empty() {
                                e.members.types.insert(decl, (child, ty));
                                e.members.note_arguments(decl, child, t, end + 1);
                            }
                        } else if first == i + 1
                            && end == stop
                            && let Some(source) = members::closure_element(t, &pairs, i)
                        {
                            rhs_uses.push((decl, source, Projection::Element));
                        }
                    }
                    first = until(t, &pairs, end, &[",", "|"]).min(stop) + 1;
                }
                if word(t, stop + 1) == "{" {
                    future.insert(stop + 1, child);
                } else {
                    let end = until(t, &pairs, stop + 1, &[",", ";", ")", "]", "}"]);
                    ends.entry(end).or_insert(scope);
                    scope = child;
                }
            }
        }
        if !excluded[i] && !matches!(prev, "." | ":") {
            if let Some((target, span)) = generics::target(t, i, &generic_scopes) {
                let id = a.calls.len();
                e.direct.insert(id, target);
                use_at[i] = Some(id);
                a.calls.push(Call {
                    span,
                    owner: e.scopes[scope].owner,
                    is_call: false,
                });
                e.uses.push(Lookup {
                    scope,
                    ns: Namespace::Type,
                    path: vec![],
                });
                excluded[i] = true;
            }
        }
        if !excluded[i] && t[i].kind == Kind::Word && ident(s) {
            let dot = prev == "." && i > 1 && word(t, i - 2) != ".";
            let label = matches!(prev, "{" | ",")
                && word(t, i + 1) == ":"
                && word(t, i + 2) != ":"
                && !e.scopes[scope].literal.is_empty();
            let shorthand = matches!(prev, "{" | ",")
                && matches!(word(t, i + 1), "," | "}")
                && !e.scopes[scope].literal.is_empty();
            let receiver = if dot {
                members::operand(t, &pairs, i - 2)
                    .and_then(|j| use_at[j])
                    .map(members::Receiver::Previous)
            } else if label || shorthand {
                Some(members::Receiver::Literal(e.scopes[scope].literal.clone()))
            } else {
                None
            };
            if let Some(receiver) = receiver {
                let id = a.calls.len();
                e.members.uses.insert(id, receiver);
                use_at[i] = Some(id);
                a.calls.push(Call {
                    span: t[i].span.clone(),
                    owner: e.scopes[scope].owner,
                    is_call: dot && call_follows(t, i),
                });
                e.uses.push(Lookup {
                    scope,
                    ns: Namespace::Either,
                    path: vec![],
                });
            }
        }
        if !excluded[i]
            && t[i].kind == Kind::Word
            && (ident(s) || matches!(s, "self" | "Self" | "crate" | "super"))
            && !matches!(prev, "'" | "$" | "#")
            && (prev != "." || i > 1 && word(t, i - 2) == ".")
            && !matches!(word(t, i + 1), "!" | "#")
            && !(word(t, i + 1) == ":" && word(t, i + 2) != ":")
        {
            let mut j = i;
            let mut path = Vec::new();
            while j >= 3
                && word(t, j - 1) == ":"
                && word(t, j - 2) == ":"
                && (ident(word(t, j - 3))
                    || matches!(word(t, j - 3), "self" | "Self" | "super" | "crate"))
            {
                j -= 3;
                path.push(name(t[j].text).into());
            }
            if !path.is_empty() {
                path.reverse();
                path.push(name(s).into());
            }
            let before = if j > 0 { word(t, j - 1) } else { "" };
            let ns = if before == ":"
                || before == "as"
                || before == ">" && j > 1 && word(t, j - 2) == "-"
            {
                Namespace::Type
            } else {
                if word(t, i + 1) == ":" && word(t, i + 2) == ":" && word(t, i + 3) != "<" {
                    Namespace::Type
                } else {
                    Namespace::Value
                }
            };
            let ns = if ns == Namespace::Value
                && name(s).starts_with(char::is_uppercase)
                && word(t, i + 1) != "("
            {
                Namespace::Either
            } else {
                ns
            };
            let ns = if before == ":"
                && j > 1
                && word(t, j - 2) != ":"
                && matches!(word(t, i + 1), "." | "(")
                && s.starts_with(char::is_lowercase)
            {
                Namespace::Value
            } else {
                ns
            };
            // A colon selects a value after a literal field and a type after
            // a declaration. Keep the surrounding delimiter across qualified paths.
            let field_rhs = before == ":"
                && j >= 3
                && matches!(word(t, j - 3), "{" | ",")
                && !e.scopes[scope].literal.is_empty()
                && delimiters.last().is_some_and(|&op| word(t, op) == "{");
            let ns = if field_rhs && word(t, i + 1) != ":" {
                Namespace::Value
            } else if s == "self" && word(t, i + 1) != ":" {
                Namespace::Value
            } else {
                ns
            };
            let is_call = call_follows(t, i);
            use_at[i] = Some(a.calls.len());
            a.calls.push(Call {
                span: t[i].span.clone(),
                owner: e.scopes[scope].owner,
                is_call,
            });
            e.uses.push(Lookup { scope, ns, path });
        }
        i += 1;
    }
    for (decl, token, projection) in rhs_uses {
        if let Some(id) = use_at[token] {
            e.members.initializers.insert(decl, (id, projection));
        }
    }
    if text_len > 0 {
        a.declarations.push(Declaration {
            span: 0..text_len,
            kind: "source_file",
        });
    }
    a.rust = Some(e);
    a
}

fn call_follows(t: &[Token], i: usize) -> bool {
    let next = word(t, i + 1);
    let mut is_call = next == "(";
    if next == ":" && word(t, i + 2) == ":" && word(t, i + 3) == "<" {
        let mut end = i + 4;
        let mut depth = 1;
        while end < t.len() && depth > 0 {
            match word(t, end) {
                "<" => depth += 1,
                ">" => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        is_call = word(t, end) == "(";
    }
    is_call
}

fn path_attribute(t: &[Token], pairs: &[usize], mut at: usize) -> Option<String> {
    if at > 0
        && word(t, at - 1) == ")"
        && pairs[at - 1] != usize::MAX
        && pairs[at - 1] > 0
        && word(t, pairs[at - 1] - 1) == "pub"
    {
        at = pairs[at - 1] - 1;
    }
    if at > 0 && word(t, at - 1) == "pub" {
        at -= 1;
    }
    while at >= 2 && word(t, at - 1) == "]" {
        let open = pairs[at - 1];
        if open == usize::MAX || open == 0 || word(t, open - 1) != "#" {
            break;
        }
        if word(t, open + 1) == "path" && word(t, open + 2) == "=" {
            let raw = word(t, open + 3);
            // An unrecognized explicit path must never select the conventional
            // same-name file. An empty name yields an unresolved module edge.
            return Some(
                raw.strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .filter(|s| !s.contains('\\'))
                    .unwrap_or("")
                    .into(),
            );
        }
        at = open - 1;
    }
    None
}
