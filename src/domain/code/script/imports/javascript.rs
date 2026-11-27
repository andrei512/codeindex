use super::*;

pub(in crate::domain::code::script) fn parse(mut rows: ImportRows<'_, '_, '_>, i: usize) -> Option<usize> {
    let t = rows.tokens;
    let mut p = i + 1;
    let mut names = Vec::new();
    if t.get(p)?.kind == Kind::String {
        return Some(p + 1);
    }
    if tok(t, p) == "type" {
        p += 1;
    }
    names.extend(default_specifier(t, &mut p)?);
    names.extend(namespace_specifier(t, &mut p)?);
    if tok(t, p) == "{" {
        named_group(t, &mut p, &mut names)?;
    }
    let module = from_module(t, &mut p)?;
    rows.record(module, names, false);
    Some(p)
}

fn default_specifier(t: &[Token], p: &mut usize) -> Option<Option<Specifier<'static>>> {
    if !word(t.get(*p)?) {
        return Some(None);
    }
    let start = *p;
    *p += 1;
    *p += usize::from(tok(t, *p) == ",");
    Some(Some(Specifier {
        imported: Some("default"),
        original: start,
        local: start,
    }))
}

fn namespace_specifier(t: &[Token], p: &mut usize) -> Option<Option<Specifier<'static>>> {
    if (tok(t, *p), tok(t, *p + 1)) != ("*", "as") {
        return Some(None);
    }
    let local = *p + 2;
    if !word(t.get(local)?) {
        return Some(None);
    }
    *p += 3;
    Some(Some(Specifier {
        imported: None,
        original: local,
        local,
    }))
}
