use super::{Analysis, Call, Declaration, Kind, Language, Token};

/// Attribute values are source spans. Skip raw-text elements so JavaScript and
/// CSS string literals that happen to contain an ID do not create HTML symbols.
pub(super) fn html(text: &str) -> Analysis {
    let b = text.as_bytes();
    let mut a = Analysis::default();
    let mut i = 0;
    while let Some(delta) = b[i..].iter().position(|&x| x == b'<') {
        i += delta + 1;
        if b[i..].starts_with(b"!--") {
            i = text[i + 3..].find("-->").map_or(b.len(), |n| i + 3 + n + 3);
            continue;
        }
        if !b.get(i).is_some_and(u8::is_ascii_alphabetic) {
            continue;
        }
        let start = i;
        while b
            .get(i)
            .is_some_and(|c| !c.is_ascii_whitespace() && !matches!(c, b'/' | b'>'))
        {
            i += 1;
        }
        let tag = &text[start..i];
        let mut mime = "";
        let mut external = false;
        while i < b.len() {
            while b.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            if i >= b.len() {
                break;
            }
            if b[i] == b'>' {
                i += 1;
                break;
            }
            let attr = i;
            while b
                .get(i)
                .is_some_and(|c| !c.is_ascii_whitespace() && !matches!(c, b'=' | b'>' | b'/'))
            {
                i += 1;
            }
            if i == attr {
                i += 1;
                continue;
            }
            let name = &b[attr..i];
            let is_id = name.eq_ignore_ascii_case(b"id");
            external |= name.eq_ignore_ascii_case(b"src");
            while b.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            if b.get(i) != Some(&b'=') {
                continue;
            }
            i += 1;
            while b.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            let quote = b.get(i).copied().filter(|c| matches!(c, b'\'' | b'"'));
            i += usize::from(quote.is_some());
            let val = i;
            while let Some(&c) = b.get(i) {
                if quote.map_or(c.is_ascii_whitespace() || c == b'>', |q| q == c) {
                    break;
                }
                i += 1;
            }
            if name.eq_ignore_ascii_case(b"type") {
                mime = &text[val..i];
            }
            if is_id && i > val {
                a.declarations.push(Declaration {
                    span: val..i,
                    kind: "attribute",
                });
            }
            i += usize::from(quote.is_some() && i < b.len());
        }
        if ["script", "style", "textarea", "title", "iframe"]
            .iter()
            .any(|k| tag.eq_ignore_ascii_case(k))
        {
            let body_start = i;
            let body_end = loop {
                let Some(n) = text[i..].find("</") else {
                    i = b.len();
                    break i;
                };
                i += n + 2;
                if b.get(i..i + tag.len())
                    .is_some_and(|s| s.eq_ignore_ascii_case(tag.as_bytes()))
                    && b.get(i + tag.len())
                        .is_some_and(|c| c.is_ascii_whitespace() || matches!(c, b'>' | b'/'))
                {
                    break i - 2;
                }
            };
            let script = tag.eq_ignore_ascii_case("script")
                && !external
                && [
                    "",
                    "module",
                    "text/javascript",
                    "application/javascript",
                    "text/ecmascript",
                    "application/ecmascript",
                    "text/typescript",
                ]
                .iter()
                .any(|v| mime.trim().eq_ignore_ascii_case(v));
            let style = tag.eq_ignore_ascii_case("style")
                && (mime.trim().is_empty() || mime.trim().eq_ignore_ascii_case("text/css"));
            if script || style {
                embedded(&mut a, &text[body_start..body_end], body_start, script);
            }
        }
    }
    a
}

// Analyze an embedded region against its own source slice and project its
// local ordinals and byte spans into the host. IDs remain HTML's public symbols.
fn embedded(host: &mut Analysis, body: &str, offset: usize, script: bool) {
    let lang = if script { Language::Js } else { Language::Css };
    let tokens = super::lexer::tokens(body, lang);
    let mut region = if script {
        super::script::analyze(lang, body, &tokens)
    } else {
        css(&tokens)
    };
    if script {
        let unit = super::Unit {
            path: "embedded.ts",
            text: body,
            analysis: &region,
        };
        let mut names = super::ForwardNames::default();
        super::script::resolve(
            &[unit],
            &std::collections::HashMap::from([("embedded.ts", 0)]),
            &mut names,
        );
        for (site, targets) in names.targets {
            region
                .direct
                .push((site.local, targets.into_iter().map(|t| t.local).collect()));
        }
    }
    let decl_base = host.declarations.len();
    let use_base = host.calls.len();
    for d in region.declarations {
        host.declarations.push(Declaration {
            span: offset + d.span.start..offset + d.span.end,
            kind: "embedded_declaration",
        });
    }
    for c in region.calls {
        host.calls.push(Call {
            span: offset + c.span.start..offset + c.span.end,
            owner: c.owner.map(|id| decl_base + id),
            is_call: c.is_call,
        });
    }
    for (site, mut targets) in region.direct {
        for t in &mut targets {
            *t += decl_base;
        }
        host.direct.push((use_base + site, targets));
    }
}
pub(super) fn css(t: &[Token<'_>]) -> Analysis {
    let mut a = Analysis::default();
    let mut prelude = 0;
    let mut scopes = vec![0];
    let mut roots = std::collections::HashSet::new();
    let mut variables = std::collections::HashMap::<(usize, &str), Vec<usize>>::new();
    let mut root_variables = std::collections::HashMap::<&str, Vec<usize>>::new();
    let mut variable_uses = Vec::new();
    let mut groups = std::collections::BTreeMap::<(usize, &str, &str), Vec<usize>>::new();
    for (i, token) in t.iter().enumerate() {
        // A CSS custom property name begins with two adjacent hyphens.
        if token.text == "-"
            && t.get(i + 1)
                .is_some_and(|n| n.text == "-" && n.span.start == token.span.end)
            && let Some(name) = t
                .get(i + 2)
                .filter(|n| n.kind == Kind::Word && n.span.start == t[i + 1].span.end)
        {
            let scope = *scopes.last().unwrap();
            let span = token.span.start..name.span.end;
            if i == prelude && scope != 0 && t.get(i + 3).is_some_and(|n| n.text == ":") {
                let id = a.declarations.len();
                a.declarations.push(Declaration {
                    span,
                    kind: "css_variable",
                });
                variables.entry((scope, name.text)).or_default().push(id);
                if roots.contains(&scope) {
                    root_variables.entry(name.text).or_default().push(id);
                }
            } else if i >= 2 && t[i - 1].text == "(" && t[i - 2].text.eq_ignore_ascii_case("var") {
                variable_uses.push((scope, name.text, span));
            }
        }
        match token.text {
            "{" => {
                let slice = &t[prelude..i];
                if scopes.len() == 1
                    && matches!(slice,[s,n] if s.text==":" && n.text.eq_ignore_ascii_case("root"))
                {
                    roots.insert(i + 1);
                }
                if !slice.first().is_some_and(|t| t.text == "@") {
                    let mut attr = 0;
                    for j in 0..slice.len() {
                        match slice[j].text {
                            "[" => attr += 1,
                            "]" => attr -= 1,
                            "." | "#" | ":" if attr == 0 => {
                                if let Some(n) = slice.get(j + 1)
                                    && n.kind == Kind::Word
                                    && n.span.start == slice[j].span.end
                                    && !n.text.starts_with(|c: char| c.is_ascii_digit())
                                {
                                    groups
                                        .entry((*scopes.last().unwrap(), slice[j].text, n.text))
                                        .or_default()
                                        .push(a.declarations.len());
                                    a.declarations.push(Declaration {
                                        span: n.span.clone(),
                                        kind: if slice[j].text != "#" {
                                            "class_name"
                                        } else {
                                            "id_name"
                                        },
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                scopes.push(i + 1);
                prelude = i + 1;
            }
            "}" => {
                if scopes.len() > 1 {
                    scopes.pop();
                }
                prelude = i + 1;
            }
            ";" => prelude = i + 1,
            _ => {}
        }
    }
    for (scope, name, span) in variable_uses {
        if let Some(targets) = variables
            .get(&(scope, name))
            .or_else(|| root_variables.get(name))
        {
            a.direct.push((a.calls.len(), targets.clone()));
        }
        a.calls.push(Call {
            span,
            owner: None,
            is_call: false,
        });
    }
    for ((_, sigil, _), ids) in groups {
        let pseudo = sigil == ":";
        for &id in &ids {
            let targets: Vec<_> = ids
                .iter()
                .copied()
                .filter(|&other| !pseudo || other != id)
                .collect();
            if targets.is_empty() {
                continue;
            }
            let d = &a.declarations[id];
            let start = d.span.start - usize::from(!pseudo);
            a.direct.push((a.calls.len(), targets));
            a.calls.push(Call {
                span: start..d.span.end,
                owner: None,
                is_call: false,
            });
        }
    }
    a
}

pub(super) fn sql(t: &[Token<'_>]) -> Analysis {
    let mut a = Analysis::default();
    for (i, tok) in t.iter().enumerate() {
        if !tok.text.eq_ignore_ascii_case("create") {
            continue;
        }
        let mut j = i + 1;
        let is = |p: usize, w: &str| t.get(p).is_some_and(|x| x.text.eq_ignore_ascii_case(w));
        if is(j, "or") && is(j + 1, "replace") {
            j += 2;
        }
        while ["temporary", "temp", "unlogged", "materialized"]
            .iter()
            .any(|w| is(j, w))
        {
            j += 1;
        }
        if !["table", "view", "procedure", "function", "sequence"]
            .iter()
            .any(|w| is(j, w))
        {
            continue;
        }
        j += 1;
        if is(j, "if") && is(j + 1, "not") && is(j + 2, "exists") {
            j += 3;
        }
        while is(j + 1, ".") {
            j += 2;
        }
        if let Some(n) = t.get(j)
            && (n.kind == Kind::Word || n.kind == Kind::String && n.text.starts_with(['"', '`']))
        {
            a.declarations.push(Declaration {
                span: n.span.clone(),
                kind: "object_reference",
            });
        }
    }
    a
}
