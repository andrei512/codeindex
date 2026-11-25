//! Recognize test registration calls at the binding used by the caller.
use super::{Kind, Role, Roles, Syntax};
use std::{
    collections::{HashMap, HashSet},
    io,
    ops::Range,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy)]
enum Binding {
    Other,
    Case,
    Suite,
    Hook,
    Namespace,
    NodeDefault,
    PlaywrightCase,
}

fn export(module: &str, name: &str) -> Binding {
    use Binding::*;
    let runner = matches!(
        module,
        "node:test"
            | "bun:test"
            | "vitest"
            | "@jest/globals"
            | "mocha"
            | "@playwright/test"
            | "ava"
    );
    if !runner {
        return Other;
    }
    match name {
        "*" => Namespace,
        "default" if module == "node:test" => NodeDefault,
        "default" | "test" if module == "@playwright/test" => PlaywrightCase,
        "default" if module == "ava" => Case,
        "default" if module == "mocha" => Namespace,
        "test" | "it" | "specify" => Case,
        "describe" | "suite" | "context" => Suite,
        "before" | "after" | "beforeEach" | "afterEach" | "beforeAll" | "afterAll" => Hook,
        _ => Other,
    }
}

fn literal<'a>(s: &'a Syntax, i: usize) -> &'a str {
    let w = s.w(i);
    if w.len() >= 2
        && matches!(w.as_bytes()[0], b'\'' | b'"')
        && w.as_bytes().last() == w.as_bytes().first()
    {
        &w[1..w.len() - 1]
    } else {
        ""
    }
}

fn ident(s: &Syntax, i: usize) -> bool {
    s.tokens
        .get(i)
        .is_some_and(|t| t.kind == Kind::Word && !t.text.starts_with(|c: char| c.is_ascii_digit()))
}

struct Scopes<'a> {
    relative: Vec<(&'a str, &'a str, &'a str)>,
    names: HashMap<&'a str, Vec<(Range<usize>, Binding)>>,
    file_end: usize,
}

impl<'a> Scopes<'a> {
    fn bind(&mut self, scope: Range<usize>, name: &'a str, binding: Binding) {
        self.names.entry(name).or_default().push((scope, binding));
    }
    fn lookup(&self, at: usize, name: &str, globals: bool) -> Binding {
        self.names
            .get(name)
            .and_then(|bindings| {
                bindings
                    .iter()
                    .rev()
                    .filter(|(scope, _)| scope.contains(&at))
                    .min_by_key(|(scope, _)| scope.len())
            })
            .map(|&(_, binding)| binding)
            .unwrap_or_else(|| {
                if globals {
                    export("mocha", name)
                } else {
                    Binding::Other
                }
            })
    }
    fn esm(&mut self, s: &'a Syntax, i: usize) {
        let mut at = i + 1;
        if !ident(s, at) && !matches!(s.w(at), "{" | "*") || s.w(at) == "type" {
            return;
        }
        let mut named = Vec::new();
        if ident(s, at) {
            named.push((s.w(at), "default"));
            at += 1;
            if s.w(at) == "," {
                at += 1
            }
        }
        if s.seq(at, &["*", "as"]) && ident(s, at + 2) {
            named.push((s.w(at + 2), "*"));
            at += 3;
        } else if s.w(at) == "{" {
            let Some(end) = s.close(at) else { return };
            at += 1;
            while at < end {
                let type_only = s.w(at) == "type" && s.w(at + 1) != "as";
                if type_only {
                    at += 1
                }
                if !ident(s, at) {
                    return;
                }
                let original = s.w(at);
                at += 1;
                let local = if s.w(at) == "as" && ident(s, at + 1) {
                    at += 2;
                    s.w(at - 1)
                } else {
                    original
                };
                if !type_only {
                    named.push((local, original));
                }
                if s.w(at) == "," {
                    at += 1
                } else if at != end {
                    return;
                }
            }
            at = end + 1;
        }
        if s.w(at) != "from" {
            return;
        }
        let module = literal(s, at + 1);
        for (local, original) in named {
            if module.starts_with("./") || module.starts_with("../") {
                self.relative.push((module, original, local));
            }
            self.bind(0..self.file_end, local, export(module, original));
        }
    }
}

// The parser's balanced pairs let us skip nested calls and callbacks in one step.
fn next_arg(s: &Syntax, mut i: usize, end: usize) -> usize {
    while i < end && s.w(i) != "," {
        i = s.close(i).unwrap_or(i) + 1;
    }
    i
}

fn callable(s: &Syntax, start: usize, end: usize) -> bool {
    if start >= end {
        return false;
    }
    if s.w(start) == "async" {
        return callable(s, start + 1, end);
    }
    if s.w(start) == "function" {
        return true;
    }
    // Identifier/member references can refer to callbacks declared elsewhere.
    if ident(s, start) && !matches!(s.w(start), "null" | "false" | "true" | "undefined") {
        let mut i = start + 1;
        while i + 1 < end && s.w(i) == "." && ident(s, i + 1) {
            i += 2
        }
        if i == end || s.seq(i, &["=", ">"]) {
            return true;
        }
    }
    s.w(start) == "("
        && s.close(start).is_some_and(|end_args| {
            s.seq(end_args + 1, &["=", ">"])
                || s.w(end_args + 1) == ":" && (end_args + 2..end).any(|i| s.seq(i, &["=", ">"]))
        })
}

fn scope(s: &Syntax, open: Option<usize>) -> Range<usize> {
    open.map_or(0..s.tokens.len(), |i| {
        i + 1..s.close(i).unwrap_or(s.tokens.len())
    })
}

fn commons_module<'a>(s: &'a Syntax, start: usize) -> Option<&'a str> {
    (s.seq(start, &["require", "("]) && s.w(start + 3) == ")")
        .then(|| literal(s, start + 2))
        .filter(|m| !m.is_empty())
}

fn declarations<'a>(s: &'a Syntax, bindings: &mut Scopes<'a>) {
    let mut braces = Vec::new();
    for (i, t) in s.tokens.iter().enumerate() {
        let area = scope(s, braces.last().copied());
        match t.text {
            "{" => braces.push(i),
            "}" => {
                braces.pop();
            }
            "import" => bindings.esm(s, i),
            "function" | "class" if ident(s, i + 1) => {
                bindings.bind(area, s.w(i + 1), Binding::Other)
            }
            "let" | "const" | "var" => {
                let mut at = i + 1;
                loop {
                    if s.w(at) == "{" {
                        let Some(end) = s.close(at) else { break };
                        let module = if s.w(end + 1) == "=" {
                            commons_module(s, end + 2)
                        } else {
                            None
                        };
                        let mut p = at + 1;
                        while p < end {
                            if ident(s, p) {
                                let alias = if s.w(p + 1) == ":" && ident(s, p + 2) {
                                    p + 2
                                } else {
                                    p
                                };
                                bindings.bind(
                                    area.clone(),
                                    s.w(alias),
                                    module.map_or(Binding::Other, |m| export(m, s.w(p))),
                                );
                            }
                            p = next_arg(s, p, end) + 1;
                        }
                        at = end + 1;
                    } else if ident(s, at) {
                        let binding = if s.w(at + 1) == "=" {
                            commons_module(s, at + 2).map_or(Binding::Other, |m| {
                                if s.w(at + 6) == "." {
                                    export(m, s.w(at + 7))
                                } else if m == "node:test" || m == "ava" {
                                    export(m, "default")
                                } else {
                                    export(m, "*")
                                }
                            })
                        } else {
                            Binding::Other
                        };
                        bindings.bind(area.clone(), s.w(at), binding);
                        at += 1;
                    } else {
                        break;
                    }
                    // Walk an initializer to the next declarator; stop at ASI
                    // or the next statement instead of consuming an entire file.
                    while at < s.tokens.len()
                        && !matches!(s.w(at), "," | ";" | ")" | "}")
                        && s.tokens[at].line == s.tokens[at.saturating_sub(1)].line
                    {
                        at = s.close(at).unwrap_or(at) + 1;
                    }
                    if s.w(at) != "," {
                        break;
                    }
                    at += 1;
                }
            }
            "(" => {
                let Some(end) = s.close(i) else { continue };
                let mut next = end + 1;
                if s.w(next) == ":" {
                    // Optional TS return annotation before the function body.
                    next += 1;
                    while next < s.tokens.len()
                        && !matches!(s.w(next), "{" | "=" | ";" | ")" | "}" | ",")
                    {
                        next = s.close(next).unwrap_or(next) + 1;
                    }
                }
                let arrow = s.seq(next, &["=", ">"]);
                let body = if arrow { next + 2 } else { next };
                let block = s.w(body) == "{";
                if !arrow
                    && (!block
                        || matches!(
                            i.checked_sub(1).map(|x| s.w(x)),
                            Some("if" | "switch" | "while" | "for" | "with")
                        ))
                {
                    continue;
                }
                let last = if block {
                    s.close(body).unwrap_or(s.tokens.len())
                } else {
                    expression_end(s, body)
                };
                let mut p = i + 1;
                while p < end {
                    if ident(s, p) {
                        bindings.bind(body..last, s.w(p), Binding::Other)
                    } else if s.w(p) == "{"
                        && let Some(close) = s.close(p)
                    {
                        let mut key = p + 1;
                        while key < close {
                            let alias = if s.w(key + 1) == ":" { key + 2 } else { key };
                            if ident(s, alias) {
                                bindings.bind(body..last, s.w(alias), Binding::Other);
                            }
                            key = next_arg(s, key, close) + 1;
                        }
                    }
                    p = next_arg(s, p, end) + 1;
                }
            }
            "=" if s.w(i + 1) == ">" && i > 0 && ident(s, i - 1) => {
                let body = i + 2;
                let end = if s.w(body) == "{" {
                    s.close(body).unwrap_or(s.tokens.len())
                } else {
                    expression_end(s, body)
                };
                bindings.bind(body..end, s.w(i - 1), Binding::Other);
            }
            _ => {}
        }
    }
}

fn expression_end(s: &Syntax, start: usize) -> usize {
    let mut i = start;
    while i < s.tokens.len() && !matches!(s.w(i), "," | ";" | ")" | "]" | "}") {
        if i > start
            && s.tokens[i].line > s.tokens[i - 1].line
            && matches!(
                s.w(i),
                "const" | "let" | "var" | "function" | "class" | "export" | "import"
            )
        {
            break;
        }
        i = s.close(i).unwrap_or(i) + 1;
    }
    i
}

// Public exports of the literal local modules queried by this source. A cycle
// produces no evidence until an actual imported test registration is found.
#[derive(Default)]
struct Modules {
    cache: HashMap<(PathBuf, String), Binding>,
    active: HashSet<PathBuf>,
}

fn module_base(parent: &Path, spec: &str) -> Option<PathBuf> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None;
    }
    let mut path = parent.parent()?.to_owned();
    for c in Path::new(spec).components() {
        match c {
            Component::Normal(name) => path.push(name),
            Component::CurDir => {}
            Component::ParentDir if path.pop() => {}
            _ => return None,
        }
    }
    Some(path)
}

impl Modules {
    fn resolve(
        &mut self,
        parent: &Path,
        spec: &str,
        which: &str,
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<Binding> {
        let Some(base) = module_base(parent, spec) else {
            return Ok(export(spec, which));
        };
        let key = (base.clone(), which.to_owned());
        if let Some(&b) = self.cache.get(&key) {
            return Ok(b);
        }
        if self.active.len() >= 64 || !self.active.insert(base.clone()) {
            return Ok(Binding::Other);
        }
        let mut paths = vec![base.clone()];
        match base.extension().and_then(|e| e.to_str()) {
            Some("js") => paths.extend(["ts", "tsx", "jsx"].map(|e| base.with_extension(e))),
            Some("mjs") => paths.push(base.with_extension("mts")),
            Some("cjs") => paths.push(base.with_extension("cts")),
            None => {
                for ext in ["ts", "tsx", "js", "jsx"] {
                    paths.push(base.with_extension(ext));
                    paths.push(base.join("index").with_extension(ext));
                }
            }
            _ => {}
        }
        let mut result = Binding::Other;
        for path in paths {
            if let Some(source) = read(&path)? {
                let t = super::lexer::tokens(&source, super::Language::Js);
                let pairs = super::script::paired_for_score(&t);
                let syntax = Syntax {
                    tokens: &t,
                    pairs: &pairs,
                    text: &source,
                };
                result = self.exported(&path, &syntax, which, read)?;
                break;
            }
        }
        self.active.remove(&base);
        self.cache.insert(key, result);
        Ok(result)
    }

    fn exported(
        &mut self,
        path: &Path,
        s: &Syntax,
        which: &str,
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<Binding> {
        let mut bindings = Scopes {
            relative: Vec::new(),
            names: HashMap::new(),
            file_end: s.tokens.len(),
        };
        declarations(s, &mut bindings);
        let mut local = None;
        let mut j = 0;
        while j < s.tokens.len() {
            if s.w(j) == "export" {
                let head = j + 1;
                match s.w(head) {
                    "default" if which == "default" => {
                        let value = head + 1;
                        if ident(s, value) {
                            local = Some(if s.w(value) == "function" {
                                s.w(value + 1)
                            } else {
                                s.w(value)
                            });
                        }
                    }
                    "function" | "const" | "let" if s.w(head + 1) == which => local = Some(which),
                    "{" => {
                        if let Some(end) = s.close(head) {
                            let mut p = head + 1;
                            while p < end {
                                let original = s.w(p);
                                let alias = if s.w(p + 1) == "as" { p + 2 } else { p };
                                if s.w(alias) == which {
                                    if s.w(end + 1) == "from" {
                                        return self.resolve(
                                            path,
                                            literal(s, end + 2),
                                            original,
                                            read,
                                        );
                                    }
                                    local = Some(original);
                                }
                                p = next_arg(s, alias, end) + 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
            j = s.close(j).unwrap_or(j) + 1;
        }
        let Some(local) = local else {
            return Ok(Binding::Other);
        };
        if let Some(&(spec, orig, _)) = bindings.relative.iter().find(|(_, _, name)| *name == local)
        {
            return self.resolve(path, spec, orig, read);
        }
        let direct = bindings.lookup(0, local, false);
        if !matches!(direct, Binding::Other) {
            return Ok(direct);
        }
        Ok(wrapper(s, &bindings, local))
    }
}

struct Function<'a> {
    name: &'a str,
    params: Vec<&'a str>,
    body: Range<usize>,
}

fn function<'a>(s: &'a Syntax, i: usize) -> Option<Function<'a>> {
    let (name, open) = match s.w(i) {
        "function" if ident(s, i + 1) && s.w(i + 2) == "(" => (s.w(i + 1), i + 2),
        "const" if ident(s, i + 1) && s.seq(i + 2, &["=", "("]) => (s.w(i + 1), i + 3),
        _ => return None,
    };
    let close = s.close(open)?;
    let mut body = close + 1;
    if s.seq(body, &["=", ">"]) {
        body += 2;
    }
    if s.w(body) != "{" {
        return None;
    }
    let end = s.close(body)?;
    let mut params = Vec::new();
    let mut p = open + 1;
    while p < close {
        while s.w(p) == "." {
            p += 1;
        }
        params.push(s.w(p));
        p = next_arg(s, p, close) + 1;
    }
    Some(Function {
        name,
        params,
        body: body + 1..end,
    })
}

// A wrapper must forward a parameter as the test title. Passing an imported
// registrar into a helper counts only if that helper actually invokes the
// corresponding formal parameter as a registrar.
fn wrapper(s: &Syntax, bindings: &Scopes, local: &str) -> Binding {
    let mut functions = Vec::new();
    let mut aliases = HashMap::new();
    let mut i = 0;
    while i < s.tokens.len() {
        if let Some(f) = function(s, i) {
            functions.push(f);
        }
        if s.w(i) == "const" && ident(s, i + 1) && s.w(i + 2) == "=" {
            let target = if s.seq(i + 3, &["Object", ".", "assign", "("]) && s.w(i + 8) == "," {
                i + 7
            } else {
                i + 3
            };
            if ident(s, target) && (target != i + 3 || matches!(s.w(target + 1), "" | ";")) {
                aliases.insert(s.w(i + 1), s.w(target));
            }
        }
        i = s.close(i).unwrap_or(i) + 1;
    }
    let mut invoked = HashMap::new();
    let mut forwards = Vec::new();
    for f in &functions {
        let mut j = f.body.start;
        while j < f.body.end {
            // Direct expression/return statements, excluding identifiers in
            // nested callbacks that are constructed but never executed.
            let mut call = j;
            if s.w(call) == "return" {
                call += 1;
            }
            if ident(s, call)
                && s.w(call + 1) == "("
                && let Some(close) = s.close(call + 1)
            {
                let mut args = Vec::new();
                let mut p = call + 2;
                while p < close {
                    args.push(p);
                    p = next_arg(s, p, close) + 1;
                }
                if args.len() >= 2 && args.iter().any(|&arg| f.params.contains(&s.w(arg))) {
                    if let Some(slot) = f.params.iter().position(|p| *p == s.w(call))
                        && let Some(title) = f.params.iter().position(|p| *p == s.w(args[0]))
                    {
                        invoked.insert(f.name, (slot, title));
                    }
                    forwards.push((f, call, args));
                }
            }
            j = s.close(j).unwrap_or(j) + 1;
        }
    }
    let mut known = HashSet::new();
    loop {
        let previous = known.len();
        for &(f, call, ref args) in &forwards {
            let callee = s.w(call);
            let runner_at = |token| {
                matches!(
                    bindings.lookup(token, s.w(token), false),
                    Binding::Case | Binding::NodeDefault | Binding::PlaywrightCase
                ) || known.contains(s.w(token))
            };
            let direct = runner_at(call) && f.params.contains(&s.w(args[0]));
            let via = invoked.get(callee).is_some_and(|&(slot, title)| {
                args.get(slot).is_some_and(|&arg| runner_at(arg))
                    && args
                        .get(title)
                        .is_some_and(|&arg| f.params.contains(&s.w(arg)))
            });
            if direct || via {
                known.insert(f.name);
            }
        }
        for (&alias, &target) in &aliases {
            if known.contains(target)
                || matches!(
                    bindings.lookup(0, target, false),
                    Binding::Case | Binding::NodeDefault | Binding::PlaywrightCase
                )
            {
                known.insert(alias);
            }
        }
        if known.len() == previous {
            break;
        }
    }
    if known.contains(local) {
        Binding::Case
    } else {
        Binding::Other
    }
}

pub(super) fn collect(
    path: &str,
    s: &Syntax,
    res: &mut Roles,
    read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
) -> io::Result<()> {
    let globals = res.base.0 == Role::Support;
    if !globals
        && !s.tokens.iter().enumerate().any(|(i, t)| {
            t.kind == Kind::String
                && (!matches!(export(literal(s, i), "*"), Binding::Other)
                    || i > 0 && s.w(i - 1) == "from" && literal(s, i).starts_with('.'))
        })
    {
        return Ok(());
    }
    let mut bindings = Scopes {
        relative: Vec::new(),
        names: HashMap::new(),
        file_end: s.tokens.len(),
    };
    declarations(s, &mut bindings);
    // Only read a local module when its imported binding is actually called
    // with a registration-shaped argument list in the current source.
    let mut modules = Modules::default();
    let imported: HashSet<_> = bindings.relative.iter().map(|&(_, _, name)| name).collect();
    let mut used = HashSet::new();
    if !imported.is_empty() {
        for i in 0..s.tokens.len() {
            if !imported.contains(s.w(i)) || i > 0 && s.w(i - 1) == "." {
                continue;
            }
            let mut opening = i + 1;
            while s.w(opening) == "."
                && matches!(
                    s.w(opening + 1),
                    "skip" | "only" | "serial" | "concurrent" | "todo"
                )
            {
                opening += 2;
            }
            if s.w(opening) != "(" {
                continue;
            }
            let Some(end) = s.close(opening) else {
                continue;
            };
            let mut arg = opening + 1;
            while arg < end {
                let close = next_arg(s, arg, end);
                if callable(s, arg, close) || (s.w(opening - 1) == "todo" && s.string(arg)) {
                    used.insert(s.w(i));
                    break;
                }
                arg = close + 1;
            }
        }
    }
    for (module, original, local) in std::mem::take(&mut bindings.relative) {
        if used.contains(local) {
            let direct = modules.resolve(Path::new(path), module, original, read)?;
            if !matches!(direct, Binding::Other) {
                bindings.bind(0..s.tokens.len(), local, direct);
            }
        }
    }
    for i in 0..s.tokens.len() {
        if !ident(s, i) || i > 0 && matches!(s.w(i - 1), "." | "function" | "class") {
            continue;
        }
        let mut binding = bindings.lookup(i, s.w(i), globals);
        let mut j = i + 1;
        let member = if matches!(binding, Binding::PlaywrightCase) {
            match s.w(j + 1) {
                "describe" => Binding::Suite,
                "beforeEach" | "afterEach" | "beforeAll" | "afterAll" => Binding::Hook,
                _ => Binding::Other,
            }
        } else {
            export("node:test", s.w(j + 1))
        };
        if s.w(j) == "."
            && (matches!(binding, Binding::Namespace)
                || matches!(binding, Binding::NodeDefault | Binding::PlaywrightCase)
                    && !matches!(member, Binding::Other))
        {
            binding = member;
            j += 2;
        }
        if matches!(binding, Binding::Namespace | Binding::Other) {
            continue;
        }
        let mut todo = false;
        while s.w(j) == "." {
            match s.w(j + 1) {
                "each" | "for" if s.w(j + 2) == "(" => {
                    let Some(close) = s.close(j + 2) else { break };
                    j = close + 1;
                }
                "skip" | "only" | "concurrent" | "serial" | "fails" | "failing" => j += 2,
                "todo" => {
                    todo = true;
                    j += 2;
                }
                _ => break,
            }
        }
        if s.w(j) != "(" {
            continue;
        }
        let Some(end) = s.close(j) else { continue };
        // Object/class method declarations also have a name followed by
        // parentheses; the following body distinguishes them from invocations.
        if s.w(end + 1) == "{" {
            continue;
        }
        let first = j + 1;
        if first >= end {
            continue;
        }
        let first_end = next_arg(s, first, end);
        let hook = matches!(binding, Binding::Hook);
        let callback = if hook || !s.string(first) {
            callable(s, first, first_end)
        } else {
            let mut arg = first_end.min(end) + 1;
            let mut found = false;
            while arg < end {
                let close = next_arg(s, arg, end);
                found |= callable(s, arg, close);
                arg = close + 1;
            }
            found || todo
        };
        if callback {
            let role = if matches!(binding, Binding::Hook | Binding::Suite) {
                Role::Support
            } else {
                Role::Test
            };
            s.add(res, i, Some(end), role, "test framework registration");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::domain::code::roles::{Role, classify};

    fn check(path: &str, source: &str, markers: &[(&str, Role)]) {
        let actual = classify(path, source);
        for &(marker, expected) in markers {
            let at = source.find(marker).unwrap();
            let role = actual
                .regions
                .iter()
                .rev()
                .find(|r| r.start <= at && at < r.end)
                .map_or(actual.base.0, |r| r.evidence.0);
            assert_eq!(role, expected, "{marker}");
        }
    }

    #[test]
    fn imported_bindings_and_local_names_are_distinct() {
        let src = r#"import {test as check} from 'node:test';
const test = (name, count) => name.length + count;
test('customer', 7);
check('registered', () => {});
function render(check: Callback): void {
    check('ordinary', () => {});
}
check('second case', {}, () => {});
"#;
        check(
            "src/components.ts",
            src,
            &[
                ("customer", Role::Production),
                ("registered", Role::Test),
                ("ordinary", Role::Production),
                ("second case", Role::Test),
            ],
        );
    }

    #[test]
    fn global_bdd_and_data_driven_cases_observe_scope() {
        let src = r#"describe('suite', function() {
  beforeEach(() => prepare());
  it.each([[1],[2]])('registered case', (value) => verify(value));
  { const it = record; it('shadowed', () => local()); }
});
class Utility { it(label, handler) { return handler(label); } }
it('invalid argument', 9);
it('outside', function() {});
"#;
        check(
            "tests/component.spec.js",
            src,
            &[
                ("prepare", Role::Support),
                ("registered case", Role::Test),
                ("shadowed", Role::Support),
                ("invalid argument", Role::Support),
                ("it(label, handler)", Role::Support),
                ("outside", Role::Test),
            ],
        );
    }

    #[test]
    fn namespace_commonjs_and_non_runner_imports() {
        let src = r#"import * as suite from 'vitest';
import {test} from './ordinary.js';
const {test: check} = require('@jest/globals');
suite.test('namespace case', async () => {});
check('commonjs case', () => {});
test('unrelated import', () => {});
const nt = require('node:test');
nt.only('node default modifier', () => {});
"#;
        check(
            "test/check.js",
            src,
            &[
                ("namespace case", Role::Test),
                ("commonjs case", Role::Test),
                ("unrelated import", Role::Support),
                ("node default modifier", Role::Test),
            ],
        );
    }
    #[test]
    fn local_reexports_require_a_forwarded_registration() {
        use crate::domain::code::roles::classify_with;
        let source = "import { caseFn, fake, warm } from './bridge.js';
caseFn('forwarded', () => {});
fake('ignored', () => {});
warm('ignored fixed title', () => {});
";
        let result = classify_with("src/main.ts", source, &mut Default::default(), |p| {
            Ok(match p.to_str().unwrap() {
                "src/bridge.ts" => {
                    Some("export { run as caseFn, fake, warm } from './adapter.js';")
                }
                "src/adapter.ts" => Some(
                    "import { test as native } from 'node:test';
function register(variant, title, body) {return variant(title, body);}
export function run(title, body) {return register(native,title,body);}
export function warm(title, body) {return register(native,'fixed',body);}
export function fake(title, body) {return [native, title, body];}",
                ),
                _ => None,
            }
            .map(String::from))
        })
        .unwrap();
        for (marker, want) in [
            ("forwarded", true),
            ("ignored", false),
            ("ignored fixed title", false),
        ] {
            let at = source.find(marker).unwrap();
            assert_eq!(
                result
                    .regions
                    .iter()
                    .any(|r| r.start <= at && at < r.end && r.evidence.0 == Role::Test),
                want,
                "{marker}"
            );
        }
    }

    #[test]
    fn ordinary_literal_calls_skip_module_io() {
        use crate::domain::code::roles::classify_with;
        classify_with(
            "src/main.ts",
            "import { load } from './unused.js'; load('data');",
            &mut Default::default(),
            |_| panic!("no callback-shaped registration"),
        )
        .unwrap();
    }

    #[test]
    fn cyclic_exports_terminate_without_creating_a_test() {
        use crate::domain::code::roles::classify_with;
        let result = classify_with(
            "src/main.ts",
            "import { check } from './a.js'; check('title', () => {});",
            &mut Default::default(),
            |p| {
                Ok(match p.to_str().unwrap() {
                    "src/a.js" => Some("export { check } from './b.js';"),
                    "src/b.js" => Some("export { check } from './a.js';"),
                    _ => None,
                }
                .map(String::from))
            },
        )
        .unwrap();
        assert!(result.regions.is_empty());
    }
}
