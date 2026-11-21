//! Static shell commands and parameter expansions. Never execute a word during lookup.
use super::{Analysis, Call, Declaration};
use crate::domain::code::{Names, Site, Unit};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Component, Path},
};
type Owner = Option<usize>;
#[derive(Default)]
pub(in crate::domain::code) struct Evidence {
    declarations: HashMap<(Owner, bool), HashMap<String, Vec<usize>>>,
    sources: Vec<(usize, Owner, String)>,
    uses: Vec<(usize, String, usize, bool)>,
    decl_scope: Vec<usize>,
    parent: Vec<usize>,
    transient: HashSet<usize>,
}
struct Parser<'a> {
    text: &'a str,
    b: &'a [u8],
    out: Analysis,
    e: Evidence,
    scope: usize,
}
#[derive(Clone, Copy, PartialEq)]
enum State {
    Command,
    Args,
    Declare,
    Export,
    Trap,
    Loop,
    Source,
    NoFunction,
}
fn initial(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}
fn id(c: u8) -> bool {
    initial(c) || c.is_ascii_digit()
}
fn separator(c: u8) -> bool {
    c.is_ascii_whitespace() || matches!(c, b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>')
}
impl Parser<'_> {
    fn prefix(&self, i: usize, s: &[u8]) -> bool {
        self.b.get(i..).is_some_and(|v| v.starts_with(s))
    }
    fn blanks(&self, mut i: usize) -> usize {
        while matches!(self.b.get(i), Some(b' ' | b'\t' | b'\r')) {
            i += 1
        }
        i
    }
    fn ident_end(&self, i: usize) -> usize {
        if self.b.get(i).is_none_or(|&b| !initial(b)) {
            return i;
        }
        let mut j = i + 1;
        while self.b.get(j).is_some_and(|&c| id(c)) {
            j += 1
        }
        j
    }
    fn declare(
        &mut self,
        name: Range<usize>,
        span: Range<usize>,
        owner: Owner,
        function: bool,
    ) -> usize {
        let key = self.text[name].to_owned();
        let local = self.out.declarations.len();
        self.e.decl_scope.push(self.scope);
        self.out.declarations.push(Declaration {
            span,
            kind: if function {
                "function_definition"
            } else {
                "shell_assignment"
            },
        });
        self.e
            .declarations
            .entry((owner, function))
            .or_default()
            .entry(key)
            .or_default()
            .push(local);
        local
    }
    fn usage(&mut self, span: Range<usize>, owner: Owner, call: bool) {
        let key = self.text[span.clone()].to_owned();
        let local = self.out.calls.len();
        self.out.calls.push(Call {
            span,
            owner,
            is_call: call,
        });
        self.e.uses.push((local, key, self.scope, call));
    }
    fn param(&mut self, i: usize, owner: Owner) -> usize {
        let braced = self.b.get(i + 1) == Some(&b'{');
        let b = i + 1 + usize::from(braced);
        let start =
            b + usize::from(braced && self.b.get(b).is_some_and(|c| matches!(c, b'#' | b'!')));
        let end = self.ident_end(start);
        if end > start {
            if braced && (self.prefix(end, b":=") || self.prefix(end, b"=")) {
                self.declare(start..end, start..end, owner, false);
            } else {
                self.usage(start..end, owner, false)
            }
        }
        if end == start { i + 1 } else { end }
    }
    fn arithmetic(&mut self, mut i: usize, owner: Owner) -> usize {
        let mut depth = 1;
        while i < self.b.len() {
            if self.prefix(i, b"))") && depth == 1 {
                return i + 2;
            }
            match self.b[i] {
                b'(' => depth += 1,
                b')' => depth = (depth - 1).max(1),
                b'$' => {
                    i = self.param(i, owner);
                    continue;
                }
                b if initial(b) => {
                    let end = self.ident_end(i);
                    let eq = self.blanks(end);
                    if self.b.get(eq) == Some(&b'=') && !self.prefix(eq, b"==") {
                        let mut last = eq + 1;
                        while self
                            .b
                            .get(last)
                            .is_some_and(|c| !matches!(c, b';' | b')' | b','))
                        {
                            last += 1
                        }
                        let mut trim = last;
                        while trim > eq + 1 && self.b[trim - 1].is_ascii_whitespace() {
                            trim -= 1
                        }
                        self.declare(i..end, i..trim, owner, false);
                    } else {
                        self.usage(i..end, owner, false)
                    }
                    i = end;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        i
    }
    fn word(&mut self, mut i: usize, owner: Owner, depth: usize) -> usize {
        let mut quote = 0;
        let mut braces = 0;
        while i < self.b.len() {
            let c = self.b[i];
            if c == b'\\' && quote != b'\'' {
                i = (i + 2).min(self.b.len());
                continue;
            }
            if matches!(c, b'\'' | b'"') && (quote == 0 || quote == c) {
                quote = if quote == 0 { c } else { 0 };
                i += 1;
                continue;
            }
            if quote != b'\'' {
                if self.prefix(i, b"$((") {
                    i = self.arithmetic(i + 3, owner);
                    continue;
                }
                if self.prefix(i, b"$(") {
                    i = self.commands(i + 2, owner, Some(b')'), depth + 1);
                    continue;
                }
                if c == b'$' {
                    if self.prefix(i, b"${") {
                        braces += 1
                    }
                    i = self.param(i, owner);
                    continue;
                }
                if c == b'}' && braces > 0 {
                    braces -= 1;
                    i += 1;
                    continue;
                }
                if c == b'`' && depth < 80 {
                    i = self.commands(i + 1, owner, Some(b'`'), depth + 1);
                    continue;
                }
            }
            if quote == 0 && braces == 0 && separator(c) {
                break;
            }
            i += 1;
        }
        i
    }
    fn here(
        &mut self,
        mut i: usize,
        delimiters: &mut Vec<(String, bool, bool)>,
        owner: Owner,
    ) -> usize {
        for (word, strip, quoted) in delimiters.drain(..) {
            while i < self.b.len() {
                let end = self.text[i..].find('\n').map_or(self.b.len(), |n| i + n);
                let line = &self.text[i..end];
                if (if strip {
                    line.trim_start_matches('\t')
                } else {
                    line
                }) == word
                {
                    i = (end + 1).min(self.b.len());
                    break;
                }
                if !quoted {
                    let mut n = i;
                    while n < end {
                        match self.b[n] {
                            b'\\' => n = (n + 2).min(end),
                            b'$' => n = self.param(n, owner),
                            _ => n += 1,
                        }
                    }
                }
                i = (end + 1).min(self.b.len());
            }
        }
        i
    }
    fn enter(&mut self) {
        let scope = self.e.parent.len();
        self.e.parent.push(self.scope);
        self.scope = scope;
    }
    fn commands(&mut self, i: usize, owner: Owner, term: Option<u8>, depth: usize) -> usize {
        let saved = self.scope;
        if depth > 0 {
            self.enter()
        }
        let pos = self.command_list(i, owner, term, depth);
        self.scope = saved;
        pos
    }
    fn command_list(
        &mut self,
        mut i: usize,
        owner: Owner,
        terminator: Option<u8>,
        depth: usize,
    ) -> usize {
        if depth > 80 {
            return self.b.len();
        }
        let mut state = State::Command;
        let mut fn_keyword = false;
        let mut cases = 0usize;
        let mut blocks = vec![];
        let mut prefix_vars = vec![];
        let mut pending_docs = vec![];
        let mut redirect = false;
        while i < self.b.len() {
            let c = self.b[i];
            if terminator == Some(c)
                && !(c == b')' && cases > 0)
                && (c != b'}' || self.b.get(i + 1).is_none_or(|&n| separator(n)))
            {
                return i + 1;
            }
            match c {
                b' ' | b'\t' | b'\r' => {
                    i += 1;
                    continue;
                }
                b'\\' if self.b.get(i + 1) == Some(&b'\n') => {
                    i += 2;
                    continue;
                }
                b'#' => {
                    i = self.text[i..].find('\n').map_or(self.b.len(), |p| i + p);
                    continue;
                }
                b'\n' => {
                    prefix_vars.clear();
                    i = self.here(i + 1, &mut pending_docs, owner);
                    state = State::Command;
                    continue;
                }
                b'(' if self.prefix(i, b"((") => {
                    i = self.arithmetic(i + 2, owner);
                    state = State::Args;
                    continue;
                }
                b'(' => {
                    redirect = false;
                    i = self.commands(i + 1, owner, Some(b')'), depth + 1);
                    state = State::Args;
                    continue;
                }
                b';' | b'|' | b'&' | b')' => {
                    prefix_vars.clear();
                    state = State::Command;
                    i += 1;
                    continue;
                }
                b'{' if state == State::Command
                    && self.b.get(i + 1).is_some_and(|&c| separator(c)) =>
                {
                    i = self.commands(i + 1, owner, Some(b'}'), depth + 1);
                    state = State::Args;
                    continue;
                }
                b'<' if self.prefix(i, b"<<") && !self.prefix(i, b"<<<") => {
                    let strip = self.b.get(i + 2) == Some(&b'-');
                    let start = self.blanks(i + 2 + usize::from(strip));
                    let mut end = start;
                    while self.b.get(end).is_some_and(|&c| !separator(c)) {
                        end += 1
                    }
                    if end > start {
                        let raw = &self.text[start..end];
                        let w = raw.replace(['\'', '"', '\\'], "");
                        let quoted = w != raw;
                        pending_docs.push((w, strip, quoted))
                    }
                    i = end;
                    continue;
                }
                b'<' | b'>' => {
                    i += 1;
                    while self
                        .b
                        .get(i)
                        .is_some_and(|c| matches!(c, b'<' | b'>' | b'&'))
                    {
                        i += 1
                    }
                    redirect = true;
                    continue;
                }
                _ => {}
            }
            let start = i;
            let end = self.word(i, owner, depth);
            if end == i {
                i += 1;
                continue;
            }
            i = end;
            let raw = &self.text[start..end];
            if redirect {
                redirect = false;
                continue;
            }
            let id_end = self.ident_end(start);
            let j = self.blanks(end);
            let mut opener = j;
            if self.prefix(j, b"()") {
                opener = self.blanks(j + 2);
                while self.b.get(opener) == Some(&b'\n') {
                    opener = self.blanks(opener + 1)
                }
            }
            let plain_cmd = raw.bytes().all(|b| id(b) || b == b'-')
                && raw.as_bytes().first().is_some_and(|&b| initial(b));
            if state == State::Command
                && plain_cmd
                && (fn_keyword || opener != j)
                && matches!(self.b.get(opener), Some(b'{' | b'('))
            {
                let d = self.declare(start..end, start..end, owner, true);
                i = self.commands(
                    opener + 1,
                    Some(d),
                    Some(if self.b[opener] == b'{' { b'}' } else { b')' }),
                    depth + 1,
                );
                fn_keyword = false;
                state = State::Args;
                continue;
            }
            fn_keyword = false;
            if matches!(state, State::Command | State::Declare | State::Export)
                && id_end > start
                && (self.b.get(id_end) == Some(&b'=') || self.prefix(id_end, b"+="))
            {
                let mut extent = end;
                if (end == id_end + 1 || end == id_end + 2 && self.prefix(id_end, b"+="))
                    && self.b.get(end) == Some(&b'(')
                {
                    extent = self.commands(end + 1, owner, Some(b')'), depth + 1);
                    i = extent;
                }
                let var = self.declare(start..id_end, start..extent, owner, false);
                if state == State::Command {
                    prefix_vars.push(var)
                }
                continue;
            }
            if state == State::Command {
                self.e.transient.extend(prefix_vars.drain(..));
            }
            match state {
                State::Command => match raw {
                    "function" => fn_keyword = true,
                    "if" | "while" | "until" => {
                        blocks.push(self.scope);
                        self.enter()
                    }
                    "then" | "else" | "elif" | "do" | "!" => (),
                    "fi" | "done" => {
                        if let Some(p) = blocks.pop() {
                            self.scope = p
                        }
                        state = State::Args
                    }
                    "for" | "select" => {
                        blocks.push(self.scope);
                        self.enter();
                        state = State::Loop
                    }
                    "case" => {
                        blocks.push(self.scope);
                        self.enter();
                        cases += 1;
                        state = State::Args
                    }
                    "esac" => {
                        if let Some(p) = blocks.pop() {
                            self.scope = p
                        }
                        cases = cases.saturating_sub(1);
                        state = State::Args
                    }
                    "trap" => state = State::Trap,
                    "export" => state = State::Export,
                    "local" | "declare" | "typeset" | "readonly" | "read" => state = State::Declare,
                    "." | "source" => state = State::Source,
                    "command" | "exec" | "builtin" | "env" | "sudo" => state = State::NoFunction,
                    _ => {
                        if plain_cmd {
                            self.usage(start..end, owner, true)
                        }
                        state = State::Args
                    }
                },
                State::Loop => {
                    if id_end == end {
                        self.declare(start..end, start..end, owner, false);
                    }
                    state = State::Args
                }
                State::Declare if raw.starts_with("-f") || raw.starts_with("-F") => {
                    state = State::Args
                }
                State::Declare => {
                    if id_end == end {
                        self.declare(start..end, start..end, owner, false);
                    }
                }
                State::Trap => {
                    if plain_cmd {
                        self.usage(start..end, owner, false);
                        self.e.uses.last_mut().unwrap().3 = true;
                    }
                    state = State::Args
                }
                State::Export => {
                    if id_end == end {
                        self.usage(start..end, owner, false)
                    }
                }
                State::Source => {
                    if let Some(path) = source_path(raw) {
                        self.e.sources.push((start, owner, path.to_string()));
                    }
                    state = State::Args
                }
                State::NoFunction | State::Args => {}
            }
        }
        i
    }
}
fn source_path(raw: &str) -> Option<&str> {
    let raw = raw.trim_matches(['"', '\'']);
    let p = raw
        .strip_prefix("$(dirname \"$0\")/")
        .or_else(|| raw.strip_prefix("${BASH_SOURCE%/*}/"))
        .unwrap_or(raw);
    (!p.is_empty()
        && !p.contains(['$', '`', '*', '?', '[', '\\', '"', '\''])
        && !Path::new(p).is_absolute())
    .then_some(p)
}
pub(super) fn analyze(text: &str) -> Analysis {
    let mut p = Parser {
        text,
        b: text.as_bytes(),
        out: Analysis::default(),
        e: Evidence {
            parent: vec![0],
            ..Default::default()
        },
        scope: 0,
    };
    p.commands(0, None, None, 0);
    p.out.direct_evidence = Some(Box::new(super::Evidence::Shell(p.e)));
    p.out
}
fn evidence<'a>(u: &'a Unit) -> Option<&'a Evidence> {
    match u.analysis.direct_evidence.as_deref()? {
        super::Evidence::Shell(s) => Some(s),
        _ => None,
    }
}
fn path_for(source: &str, import: &str, paths: &HashMap<&str, usize>) -> Option<usize> {
    let mut p = Path::new(source).parent()?.to_owned();
    for comp in Path::new(import).components() {
        match comp {
            Component::Normal(n) => p.push(n),
            Component::CurDir => {}
            Component::ParentDir => {
                if !p.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    paths
        .get(p.to_str()?)
        .or_else(|| paths.get(import))
        .copied()
}
type SourceEdges = HashMap<usize, Vec<(usize, Owner, usize)>>;
type Exports = HashMap<(bool, String), Site>;

fn exports(
    file: usize,
    units: &[Unit],
    sources: &SourceEdges,
    visiting: &mut HashSet<usize>,
    cache: &mut HashMap<usize, Exports>,
) -> bool {
    if cache.contains_key(&file) {
        return true;
    }
    if visiting.len() >= 80 || !visiting.insert(file) {
        return false;
    }
    let Some(e) = evidence(&units[file]) else {
        visiting.remove(&file);
        return false;
    };
    let mut table = HashMap::<(bool, String), (usize, Site)>::new();
    for (&(owner, function), names) in &e.declarations {
        if owner.is_some() {
            continue;
        }
        for (name, ids) in names {
            if let Some(&local) = ids.iter().rev().find(|id| !e.transient.contains(id)) {
                table.insert(
                    (function, name.clone()),
                    (
                        units[file].analysis.declarations[local].span.start,
                        Site { file, local },
                    ),
                );
            }
        }
    }
    for &(pos, owner, target) in sources.get(&file).into_iter().flatten() {
        if owner.is_some() {
            continue;
        }
        if !exports(target, units, sources, visiting, cache) {
            visiting.remove(&file);
            return false;
        }
        for (key, &site) in &cache[&target] {
            if table.get(key).is_none_or(|(now, _)| *now < pos) {
                table.insert(key.clone(), (pos, site));
            }
        }
    }
    visiting.remove(&file);
    cache.insert(
        file,
        table
            .into_iter()
            .map(|(name, (_, site))| (name, site))
            .collect(),
    );
    true
}
pub(super) fn resolve(units: &[Unit], paths: &HashMap<&str, usize>, names: &mut Names) {
    let sources: SourceEdges = units
        .iter()
        .enumerate()
        .filter_map(|(f, u)| {
            let e = evidence(u)?;
            Some((
                f,
                e.sources
                    .iter()
                    .filter_map(|(pos, owner, p)| Some((*pos, *owner, path_for(u.path, p, paths)?)))
                    .collect(),
            ))
        })
        .collect();
    let mut libraries = HashMap::new();
    let mut visiting = HashSet::new();
    for (_, _, f) in sources.values().flatten() {
        exports(*f, units, &sources, &mut visiting, &mut libraries);
    }
    for (file, u) in units.iter().enumerate() {
        let Some(e) = evidence(u) else { continue };
        for (id, key, region, function) in &e.uses {
            let at = Site { file, local: *id };
            let c = &u.analysis.calls[*id];
            let mut hit = None;
            for own in [c.owner, None] {
                if let Some(items) = e
                    .declarations
                    .get(&(own, *function))
                    .and_then(|by_name| by_name.get(key))
                {
                    let mut preceding = items.iter().copied().filter(|&id| {
                        u.analysis.declarations[id].span.start <= c.span.start
                            && !e.transient.contains(&id)
                    });
                    let nearest = preceding
                        .clone()
                        .filter(|&id| {
                            let target = e.decl_scope[id];
                            let mut r = *region;
                            while r != target && r != 0 {
                                r = e.parent[r]
                            }
                            r == target
                        })
                        .max_by_key(|&id| u.analysis.declarations[id].span.start);
                    let local = nearest.or_else(|| preceding.next_back()).or_else(|| {
                        (*function && c.owner.is_some() && own.is_none())
                            .then(|| items.first().copied())
                            .flatten()
                    });
                    if let Some(local) = local {
                        hit = Some((
                            u.analysis.declarations[local].span.start,
                            Site { file, local },
                        ));
                        break;
                    }
                }
                if own.is_none() {
                    break;
                }
            }
            for &(pos, owner, f) in sources[&file].iter().rev() {
                if pos >= c.span.start || owner.is_some() && owner != c.owner {
                    continue;
                }
                if hit.is_some_and(|(before, _)| before > pos) {
                    break;
                }
                if let Some(site) = libraries
                    .get(&f)
                    .and_then(|t| t.get(&(*function, key.clone())))
                {
                    hit = Some((pos, *site));
                    break;
                }
            }
            if let Some((_, target)) = hit {
                names.bind(at, vec![target])
            }
        }
    }
}

crate::domain::code::cache::record!(Evidence { declarations, sources, uses, decl_scope, parent, transient });
