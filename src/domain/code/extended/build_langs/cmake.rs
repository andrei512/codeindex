use crate::domain::code::lexer::cmake_bracket as bracket;
use super::{Facts, simple, span, target};
use crate::domain::code::Analysis;

#[derive(Clone, Copy, PartialEq)]
enum Form {
    Word,
    Quoted,
    Bracket,
    Open,
    Close,
}
#[derive(Clone, Copy)]
struct Arg<'a> {
    text: &'a str,
    form: Form,
}

fn tokens(mut s: &str) -> Vec<Arg<'_>> {
    let mut out = Vec::new();
    while !s.is_empty() {
        s = s.trim_start_matches(char::is_whitespace);
        let Some(&c) = s.as_bytes().first() else {
            break;
        };
        if c == b'#' {
            s = if let Some((_, end)) = bracket(&s[1..]) {
                &s[1 + end..]
            } else {
                s.split_once('\n').map_or("", |(_, rest)| rest)
            };
            continue;
        }
        if let Some((_, end)) = bracket(s) {
            out.push(Arg {
                text: &s[..end],
                form: Form::Bracket,
            });
            s = &s[end..];
            continue;
        }
        if c == b'(' || c == b')' {
            out.push(Arg {
                text: &s[..1],
                form: if c == b'(' { Form::Open } else { Form::Close },
            });
            s = &s[1..];
            continue;
        }
        let quoted = c == b'"';
        let start = usize::from(quoted);
        let mut escaped = false;
        let mut end = s.len();
        let mut consumed = end;
        for (i, ch) in s[start..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if quoted && ch == '"'
                || !quoted && (ch.is_whitespace() || matches!(ch, '(' | ')' | '#'))
            {
                end = start + i;
                consumed = end + usize::from(quoted);
                break;
            }
        }
        out.push(Arg {
            text: &s[start..end],
            form: if quoted { Form::Quoted } else { Form::Word },
        });
        s = &s[consumed..];
    }
    out
}

pub(super) fn analyze(source: &str) -> Analysis {
    let tokens = tokens(source);
    let mut f = Facts::default();
    let mut owners = Vec::new();
    let mut i = 0;
    while i + 1 < tokens.len() {
        if tokens[i].form != Form::Word || tokens[i + 1].form != Form::Open {
            i += 1;
            continue;
        }
        let cmd = tokens[i].text;
        let mut depth = 1usize;
        let start = i + 2;
        i = start;
        while i < tokens.len() {
            match tokens[i].form {
                Form::Open => depth += 1,
                Form::Close => depth -= 1,
                _ => (),
            }
            if depth == 0 {
                break;
            }
            i += 1;
        }
        if i == tokens.len() {
            break;
        }
        let args = &tokens[start..i];
        i += 1;
        let op = cmd.to_ascii_lowercase();
        let first = args
            .first()
            .filter(|a| matches!(a.form, Form::Quoted | Form::Word))
            .map(|a| a.text);
        let owner = owners.last().copied().flatten();
        if matches!(&*op, "function" | "macro") {
            let own = first.filter(|s| simple(s)).map(|s| {
                f.declare(
                    format!("f:{}", s.to_ascii_lowercase()),
                    span(source, s),
                    "function_definition",
                )
            });
            owners.push(own);
            for arg in args
                .iter()
                .skip(1)
                .filter(|a| a.form == Form::Word && simple(a.text))
            {
                f.declare(
                    format!("v:{own:?}:{}", arg.text),
                    span(source, arg.text),
                    "parameter",
                );
            }
            continue;
        }
        if matches!(&*op, "endfunction" | "endmacro") {
            owners.pop();
            continue;
        }
        let mut arg_owner = owner;
        let mut variable = None;
        let properties = args
            .iter()
            .position(|a| a.text == "PROPERTIES")
            .unwrap_or(args.len());
        if matches!(&*op, "add_executable" | "add_library" | "add_custom_target") {
            if let Some(s) = first.filter(|s| target(s)) {
                arg_owner = Some(f.declare(format!("t:{s}"), span(source, s), "build_target"));
            }
        } else if matches!(&*op, "set" | "option") {
            if args.len() > 1 && args.last().is_none_or(|v| v.text != "PARENT_SCOPE") {
                variable = first.filter(|s| simple(s));
            }
        } else {
            f.usage(format!("f:{op}"), span(source, cmd), owner, true, true);
        }
        let target_owner = if matches!(op.as_str(), "target_link_libraries" | "add_dependencies") {
            first
                .and_then(|name| f.names.get(&format!("t:{name}")))
                .and_then(|ids| match &ids[..] {
                    [id] => Some(*id),
                    _ => None,
                })
        } else {
            None
        };
        for (j, arg) in args
            .iter()
            .enumerate()
            .filter(|(_, a)| matches!(a.form, Form::Word | Form::Quoted))
        {
            let s = arg.text;
            let target_ref = match &*op {
                "target_link_libraries" | "add_dependencies" => true,
                "add_library" | "add_executable" => {
                    j == 2 && args.get(1).is_some_and(|a| a.text == "ALIAS")
                }
                "set_target_properties" => j < properties,
                "target_sources"
                | "target_compile_definitions"
                | "target_compile_features"
                | "target_compile_options"
                | "target_include_directories"
                | "target_link_directories"
                | "target_link_options"
                | "target_precompile_headers" => j == 0,
                _ => false,
            };
            if target_ref
                && target(s)
                && !matches!(
                    s,
                    "PRIVATE" | "PUBLIC" | "INTERFACE" | "debug" | "optimized" | "general"
                )
            {
                let owner = if j == 0 {
                    arg_owner
                } else {
                    target_owner.or(arg_owner)
                };
                f.usage(format!("t:{s}"), span(source, s), owner, false, false);
            }
            let mut offset = 0;
            while let Some(pos) = s[offset..].find("${") {
                let pos = offset + pos;
                offset = pos + 2;
                if s[..pos].bytes().rev().take_while(|&b| b == b'\\').count() % 2 != 0 {
                    continue;
                }
                let Some(end) = s[offset..].find('}') else {
                    break;
                };
                let name = &s[offset..offset + end];
                if simple(name) {
                    let local = format!("v:{owner:?}:{name}");
                    // A function can inherit a variable from its caller. Bind inside
                    // functions only when this lexical body supplies its own value.
                    if f.names.contains_key(&local) {
                        f.usage(local, span(source, name), arg_owner, false, true);
                    }
                }
            }
        }
        if let Some(name) = variable {
            f.declare(
                format!("v:{owner:?}:{name}"),
                span(source, name),
                "variable_declarator",
            );
        }
    }
    f.finish()
}
