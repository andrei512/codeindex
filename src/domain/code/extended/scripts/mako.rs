use super::{
    Analysis, call, declare,
    lex::{identifier, quoted},
};

fn python_string(s: &str, q: u8) -> usize {
    if s.as_bytes().get(..3) == Some(&[q, q, q]) {
        let b = s.as_bytes();
        let mut i = 3;
        while i < b.len() {
            if b.get(i..i + 3) == Some(&[q, q, q]) {
                return i + 3;
            }
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        b.len()
    } else {
        quoted(s, q)
    }
}

fn python_end(s: &str, expression: bool) -> usize {
    let mut i = 2;
    let mut braces = 1usize;
    while i < s.len() {
        let c = s.as_bytes()[i];
        if !expression && s.as_bytes()[i..].starts_with(b"%>") {
            return i + 2;
        }
        match c {
            b'\'' | b'"' => i += python_string(&s[i..], c),
            b'#' => i += s[i..].find('\n').unwrap_or(s.len() - i),
            b'{' if expression => {
                braces += 1;
                i += 1;
            }
            b'}' if expression => {
                braces -= 1;
                i += 1;
                if braces == 0 {
                    break;
                }
            }
            _ => i += 1,
        }
    }
    i
}

fn tag<'a>(source: &'a str, start: usize, out: &mut Analysis, args: &mut Option<&'a str>) -> usize {
    let s = &source[start..];
    let mut i = 2;
    let n = identifier(&s[i..]);
    let tag = &s[i..i + n];
    i += n;
    let kind = match tag {
        "def" => Some("function_definition"),
        "block" => Some("block_declaration"),
        "namespace" => Some("module_declaration"),
        _ => None,
    };
    let mut name = None;
    while i < s.len() {
        let r = &s[i..];
        if r.starts_with('>') {
            i += 1;
            break;
        }
        if r.starts_with(char::is_whitespace) {
            i += r.chars().next().unwrap().len_utf8();
            continue;
        }
        if r.starts_with('/') {
            i += 1;
            continue;
        }
        let len = identifier(r);
        if len == 0 {
            return i + r.chars().next().unwrap().len_utf8();
        }
        let key = &r[..len];
        i += len;
        i = s.len() - s[i..].trim_start().len();
        if !s[i..].starts_with('=') {
            continue;
        }
        i += 1;
        i = s.len() - s[i..].trim_start().len();
        let Some(&q) = s.as_bytes().get(i).filter(|&&q| q == b'\'' || q == b'"') else {
            continue;
        };
        let width = quoted(&s[i..], q);
        if key == "name" && width >= 2 && s.as_bytes()[i + width - 1] == q {
            let val = &s[i + 1..i + width - 1];
            let n = identifier(val);
            if n > 0
                && ((tag == "def" && val[n..].trim_start().starts_with('('))
                    || (tag != "def" && n == val.len()))
            {
                name = Some(start + i + 1..start + i + 1 + n);
                if tag == "def" {
                    *args = val[n..]
                        .trim()
                        .strip_prefix('(')
                        .and_then(|s| s.strip_suffix(')'));
                }
            }
        }
        if key == "args" && width >= 2 && s.as_bytes()[i + width - 1] == q {
            *args = Some(&s[i + 1..i + width - 1]);
        }
        i += width;
    }
    if let (Some(kind), Some(span)) = (kind, name) {
        declare(out, span, kind);
    }
    if matches!(tag, "doc" | "text") {
        let close = if tag == "doc" { "</%doc>" } else { "</%text>" };
        return s[i..].find(close).map_or(s.len(), |n| i + n + close.len());
    }
    i
}

#[derive(Default)]
struct Scope<'a> {
    parent: Option<usize>,
    owner: Option<usize>,
    bindings: std::collections::HashMap<&'a str, Option<usize>>,
}

fn record_params<'a>(args: &'a str, scope: &mut Scope<'a>) {
    let (mut depth, mut first, mut p) = (0usize, true, 0);
    while p < args.len() {
        let tail = &args[p..];
        let byte = tail.as_bytes()[0];
        let id = identifier(tail);
        if id > 0 {
            if first {
                scope.bindings.insert(&tail[..id], None);
                first = false;
            }
            p += id;
            continue;
        }
        match byte {
            b'\'' | b'"' => {
                p += python_string(tail, byte);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => first = true,
            _ => {}
        }
        p += tail.chars().next().unwrap().len_utf8();
    }
}

fn record_writes<'a>(code: &'a str, scope: &mut Scope<'a>) {
    let mut p = 0;
    let mut binding = false;
    let mut depth = 0usize;
    let mut attr = false;
    while p < code.len() {
        let tail = &code[p..];
        let byte = tail.as_bytes()[0];
        let id = identifier(tail);
        if id > 0 {
            let name = &tail[..id];
            let next = tail[id..].trim_start();
            if !attr && (binding || depth == 0 && next.starts_with('=') && !next.starts_with("=="))
            {
                scope.bindings.insert(name, None);
            }
            binding = depth == 0 && matches!(name, "for" | "as" | "def" | "class" | "import");
            attr = false;
            p += id;
        } else if matches!(byte, b'\'' | b'"') {
            p += python_string(tail, byte);
            binding = false;
        } else if byte == b'#' {
            p += tail.find('\n').unwrap_or(tail.len());
        } else {
            p += tail.chars().next().unwrap().len_utf8();
            if !byte.is_ascii_whitespace() {
                match byte {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                attr = byte == b'.';
                binding = false;
            }
        }
    }
}

pub(super) fn analyze(source: &str) -> Analysis {
    let mut out = Analysis::default();
    let mut scopes = vec![Scope::default()];
    let mut active = vec![("", 0)];
    let mut defs = std::collections::HashMap::new();
    let mut pending = Vec::new();
    let mut at = 0;
    while at < source.len() {
        let rest = &source[at..];
        let scope = active.last().unwrap().1;
        if rest.starts_with("##")
            || (rest.starts_with('%')
                && !rest.starts_with("%%")
                && source[..at]
                    .rsplit_once('\n')
                    .map_or(&source[..at], |(_, l)| l)
                    .trim_matches([' ', '\t'])
                    .is_empty())
        {
            let line = rest.find('\n').unwrap_or(rest.len());
            if rest.starts_with('%') {
                record_writes(&rest[..line], &mut scopes[scope]);
            }
            at += line;
        } else if rest.starts_with("<!--") {
            at += rest.find("-->").map_or(rest.len(), |n| n + 3);
        } else if rest.starts_with("${") {
            let end = python_end(rest, true);
            let expression = rest[2..end].trim_start();
            {
                let member = expression.strip_prefix("local.").unwrap_or(expression);
                let len = identifier(member);
                if len > 0 && member[len..].trim_start().starts_with('(') {
                    let start = at + 2 + rest[2..end].len() - member.len();
                    pending.push((start..start + len, scope, expression.starts_with("local.")));
                }
            }
            at += end;
        } else if let Some(suffix) = rest.strip_prefix("</%") {
            let name = &suffix[..identifier(suffix)];
            if suffix[name.len()..].starts_with('>')
                && active.len() > 1
                && active.last().is_some_and(|s| s.0 == name)
            {
                active.pop();
            }
            at += 3 + name.len();
        } else if let Some(tail) = rest.strip_prefix("<%") {
            if tail.starts_with(|c: char| c.is_ascii_alphabetic()) {
                let name = &tail[..identifier(tail)];
                let previous = out.declarations.len();
                let mut args = None;
                let len = tag(source, at, &mut out, &mut args);
                let declared = out.declarations.get(previous);
                if name == "def"
                    && let Some(d) = declared
                {
                    let member = &source[d.span.clone()];
                    scopes[scope]
                        .bindings
                        .entry(member)
                        .and_modify(|v| *v = None)
                        .or_insert(Some(previous));
                    if scope == 0 {
                        defs.entry(member)
                            .and_modify(|v| *v = None)
                            .or_insert(Some(previous));
                    }
                }
                if matches!(name, "def" | "block" | "namespace" | "call")
                    && !rest[..len].ends_with("/>")
                {
                    let child = scopes.len();
                    scopes.push(Scope {
                        parent: Some(scope),
                        owner: declared.map(|_| previous),
                        ..Default::default()
                    });
                    active.push((name, child));
                    if let Some(args) = args {
                        record_params(args, &mut scopes[child]);
                    }
                } else if name == "page"
                    && let Some(args) = args
                {
                    record_params(args, &mut scopes[scope]);
                }
                at += len;
            } else {
                let end = python_end(rest, false);
                let block = &rest[2..end];
                record_writes(
                    block.strip_suffix("%>").unwrap_or(block),
                    &mut scopes[scope],
                );
                at += end;
            }
        } else {
            at += rest.chars().next().unwrap().len_utf8();
        }
    }
    for (span, scope, qualified) in pending {
        let name = &source[span.clone()];
        let binding = if qualified {
            defs.get(name)
        } else {
            std::iter::successors(Some(scope), |&s| scopes[s].parent)
                .find_map(|s| scopes[s].bindings.get(name))
        };
        if let Some(Some(id)) = binding {
            call(&mut out, span, scopes[scope].owner, *id);
        }
    }
    out
}
