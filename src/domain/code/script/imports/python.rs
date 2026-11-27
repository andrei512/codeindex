use super::*;

pub(in crate::domain::code::script) fn parse(mut rows: ImportRows<'_, '_, '_>, i: usize) -> Option<usize> {
    let (t, scope) = (rows.tokens, rows.scope);
    if tok(t, i) != "from" {
        return rows.python_modules(i + 1);
    }
    let (module, mut p) = from_clause(t, i)?;
    if tok(t, p) == "*" {
        let id = rows.evidence.import_route(module, None);
        rows.evidence.wildcards.push((scope, id));
        return Some(p + 1);
    }
    let names = from_members(t, &mut p)?;
    rows.record(&module, names, scope == 0);
    Some(p)
}

fn from_clause(t: &[Token], keyword: usize) -> Option<(String, usize)> {
    let start = keyword + 1;
    let line = t[keyword].line;
    let end = start
        + t[start..]
            .iter()
            .take_while(|part| {
                part.line == line && part.text != "import" && (word(part) || part.text == ".")
            })
            .count();
    if end == start || tok(t, end) != "import" {
        return None;
    }
    Some((
        t[start..end].iter().map(|token| token.text).collect(),
        end + 1,
    ))
}

fn from_members<'a>(t: &[Token<'a>], cursor: &mut usize) -> Option<Vec<Specifier<'a>>> {
    let mut names = Vec::new();
    let grouped = tok(t, *cursor) == "(";
    *cursor += usize::from(grouped);
    loop {
        names.push(named(t, cursor)?);
        if !next_member(t, cursor, grouped) {
            break;
        }
    }
    if grouped && tok(t, *cursor) != ")" {
        return None;
    }
    *cursor += usize::from(grouped);
    Some(names)
}

fn next_member(t: &[Token], cursor: &mut usize, grouped: bool) -> bool {
    if tok(t, *cursor) != "," {
        return false;
    }
    *cursor += 1;
    !(grouped && tok(t, *cursor) == ")")
}

impl ImportRows<'_, '_, '_> {
    fn python_modules(&mut self, mut p: usize) -> Option<usize> {
        loop {
            let (written, local) = module_name(self.tokens, &mut p)?;
            self.python_module(written, local);
            if tok(self.tokens, p) != "," {
                return Some(p);
            }
            p += 1;
        }
    }

    fn python_module(&mut self, written: std::ops::Range<usize>, local: Option<usize>) {
        let root = tok(self.tokens, written.start);
        let path: String = self.tokens[written].iter().map(|tok| tok.text).collect();
        if let Some(local) = local {
            let index = self.evidence.import_route(path, None);
            self.evidence
                .declare(self.scope, tok(self.tokens, local), Binding::Import(index));
        } else {
            self.evidence.package_import(self.scope, root, path);
        }
    }
}

impl Evidence {
    fn package_import(&mut self, scope: usize, root: &str, path: String) {
        let import = self.import_route(&path, None);
        self.packages.insert_owned(scope, path, import);
        self.scopes[scope]
            .bindings
            .entry(root.to_owned())
            .or_insert(Binding::Package);
    }
}

/// Python binds the root of an unaliased dotted import. A caller that
/// represents package roots separately can distinguish that case by `None`.
pub(in crate::domain::code) fn module_name(
    t: &[Token],
    p: &mut usize,
) -> Option<(std::ops::Range<usize>, Option<usize>)> {
    let start = *p;
    if !word(t.get(start)?) {
        return None;
    }
    let end = super::super::syntax::dotted_name_end(t, start);
    *p = end;
    let local = alias(t, p).or_else(|| (end == start + 1).then_some(start));
    Some((start..end, local))
}

pub(in crate::domain::code) fn member_name(t: &[Token], p: &mut usize) -> Option<(usize, usize)> {
    let imported = named(t, p)?;
    Some((imported.original, imported.local))
}
