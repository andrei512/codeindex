use super::{
    Analysis, Binding, Evidence, Import, Kind, Language, Lookup, Token, UseRequest, tok, word,
};
impl Evidence {
    fn import_route(&mut self, module: impl Into<String>, name: Option<&str>) -> usize {
        let id = self.imports.len();
        self.imports.push(Import {
            module: module.into(),
            name: name.map(str::to_owned),
        });
        id
    }
}

struct Specifier<'a> {
    imported: Option<&'a str>,
    original: usize,
    local: usize,
}

fn quoted(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let [first, .., last] = b else { return None };
    if !matches!(first, b'\'' | b'"') || first != last {
        return None;
    }
    let value = &s[1..s.len() - 1];
    (!value.contains('\\')).then_some(value)
}
/// Return the module spelling and commit the cursor only for a complete from-clause.
fn from_module<'a>(t: &[Token<'a>], p: &mut usize) -> Option<&'a str> {
    if tok(t, *p) != "from" {
        return None;
    }
    let module = quoted(tok(t, *p + 1))?;
    *p += 2;
    Some(module)
}

/// Python wildcard exports are provable only for a single literal __all__ list.
pub(super) fn python_exports(t: &[Token], pairs: &[usize], name: usize, evidence: &mut Evidence) {
    let candidate = evidence
        .all
        .is_none()
        .then(|| python_export_items(t, pairs, name))
        .flatten();
    let Some(items) = candidate else {
        evidence.dynamic_all = true;
        return;
    };
    let mut names = Vec::new();
    for token in items.iter().filter(|token| token.text != ",") {
        let Some(name) = python_export_name(token) else {
            evidence.dynamic_all = true;
            break;
        };
        names.push(name.to_owned());
    }
    evidence.all = Some(names);
}

fn python_export_items<'t, 's>(
    t: &'t [Token<'s>],
    pairs: &[usize],
    name: usize,
) -> Option<&'t [Token<'s>]> {
    let open = name + 2;
    if tok(t, name + 1) != "=" || !matches!(tok(t, open), "[" | "(") {
        return None;
    }
    let end = pairs[open];
    if end >= t.len() {
        return None;
    }
    t.get(open + 1..end)
}

fn python_export_name<'s>(token: &Token<'s>) -> Option<&'s str> {
    let text = token.text;
    if token.kind != Kind::String || text.len() < 2 || text.contains('\\') {
        return None;
    }
    Some(&text[1..text.len() - 1])
}

pub(super) mod javascript;
pub(super) mod python;

fn alias(t: &[Token], p: &mut usize) -> Option<usize> {
    if tok(t, *p) == "as" && t.get(*p + 1).is_some_and(word) {
        *p += 2;
        Some(*p - 1)
    } else {
        None
    }
}

fn named<'s>(t: &[Token<'s>], p: &mut usize) -> Option<Specifier<'s>> {
    let original = *p;
    if !word(t.get(original)?) {
        return None;
    }
    *p += 1;
    let local = alias(t, p).unwrap_or(original);
    Some(Specifier {
        imported: Some(tok(t, original)),
        original,
        local,
    })
}

/// Both import and export specifier lists allow type markers and renamed bindings.
pub(in crate::domain::code) fn js_named_specifiers(
    t: &[Token],
    p: &mut usize,
    mut visit: impl FnMut(usize, usize, bool),
) -> Option<()> {
    *p += 1;
    while tok(t, *p) != "}" {
        let type_only = tok(t, *p) == "type" && tok(t, *p + 1) != "as";
        *p += usize::from(type_only);
        let item = named(t, p)?;
        visit(item.original, item.local, type_only);
        match tok(t, *p) {
            "," => *p += 1,
            "}" => break,
            _ => return None,
        }
    }
    *p += 1;
    Some(())
}

fn named_group<'s>(t: &[Token<'s>], p: &mut usize, names: &mut Vec<Specifier<'s>>) -> Option<()> {
    js_named_specifiers(t, p, |original, local, _| {
        names.push(Specifier {
            imported: Some(tok(t, original)),
            original,
            local,
        });
    })
}

pub(super) struct ImportRows<'t, 's, 'out> {
    tokens: &'t [Token<'s>],
    scope: usize,
    analysis: &'out mut Analysis,
    evidence: &'out mut Evidence,
}

impl<'t, 's, 'out> ImportRows<'t, 's, 'out> {
    pub(super) fn new(tokens: &'t [Token<'s>], scope: usize, analysis: &'out mut Analysis, evidence: &'out mut Evidence) -> Self {
        Self { tokens, scope, analysis, evidence }
    }

    pub(super) fn exports(&mut self, at: usize) -> Option<usize> {
        if self.scope != 0 {
            return None;
        }
        let (export, after) = Export::read(self.tokens, at + 1)?;
        self.export(export);
        Some(after)
    }
}

impl ImportRows<'_, '_, '_> {
    fn record(&mut self, module: &str, names: Vec<Specifier<'_>>, reexport: bool) {
        for Specifier {
            imported,
            original,
            local,
        } in names
        {
            let n = self.evidence.import_route(module, imported);
            let binding = match imported {
                None => Binding::Namespace {
                    import: n,
                    local: self
                        .analysis
                        .declare_at(self.tokens[local].span.clone(), "local_binding"),
                },
                Some(_) => Binding::Import(n),
            };
            let spelling = tok(self.tokens, local);
            self.evidence.declare(self.scope, spelling, binding);
            if reexport {
                self.evidence
                    .forward
                    .insert(spelling.to_owned(), super::Forward::Import(n));
            }
            if imported.is_some() {
                self.reference(original, Lookup::Imported(n));
                if local != original {
                    self.reference(local, Lookup::Imported(n));
                }
            }
        }
    }

    fn reference(&mut self, token: usize, lookup: Lookup) {
        UseRequest::reference(self.tokens[token].span.clone(), self.scope, lookup)
            .publish(self.analysis, self.evidence);
    }
}
// Only a complete identifier expression forwards its binding. A newline before
// a call, subscript, tagged template or binary operator continues the expression.
fn default_local(t: &[Token], at: usize) -> Option<usize> {
    let local = at + 1;
    let token = t.get(local)?;
    let identifier = word(token) && super::NameKind::of(Language::Js, token.text).identifier();
    if tok(t, at) != "default" || !identifier {
        return None;
    }
    let Some(next) = t.get(local + 1) else { return Some(local) };
    if next.text == ";" {
        return Some(local);
    }
    (next.line > token.line && !continues_value(t, local + 1)).then_some(local)
}

fn continues_value(t: &[Token], at: usize) -> bool {
    let next = &t[at];
    let adjacent = t.get(at + 1).filter(|token| next.span.end == token.span.start);
    match next.kind {
        Kind::String => next.text.starts_with('`'),
        Kind::Word => matches!(next.text, "in" | "instanceof" | "as" | "satisfies"),
        Kind::Punct => match next.text {
            "+" | "-" => adjacent.is_none_or(|token| token.text != next.text),
            "!" => adjacent.is_some_and(|token| token.text == "="),
            value => matches!(value, "(" | "[" | "." | "?" | "," | "=" | "*" | "/" | "%" | "&" | "|" | "^" | "<" | ">"),
        },
    }
}

enum Export<'a> {
    Default(usize),
    Star {
        module: &'a str,
        namespace: Option<&'a str>,
    },
    Named {
        module: Option<&'a str>,
        items: Vec<Specifier<'a>>,
    },
}

impl<'s> Export<'s> {
    fn read(t: &[Token<'s>], mut p: usize) -> Option<(Self, usize)> {
        if let Some(local) = default_local(t, p) {
            return Some((Self::Default(local), local + 1));
        }
        p += usize::from(tok(t, p) == "type");
        let export = match tok(t, p) {
            "*" => Self::star(t, &mut p)?,
            "{" => Self::named(t, &mut p)?,
            _ => return None,
        };
        Some((export, p))
    }

    fn star(t: &[Token<'s>], p: &mut usize) -> Option<Self> {
        *p += 1;
        let namespace = if tok(t, *p) == "as" {
            Some(tok(t, alias(t, p)?))
        } else {
            None
        };
        Some(Self::Star { module: from_module(t, p)?, namespace })
    }

    fn named(t: &[Token<'s>], p: &mut usize) -> Option<Self> {
        let mut items = Vec::new();
        named_group(t, p, &mut items)?;
        let module = if tok(t, *p) == "from" { Some(from_module(t, p)?) } else { None };
        Some(Self::Named { module, items })
    }
}

impl ImportRows<'_, '_, '_> {
    fn export(&mut self, export: Export<'_>) {
        match export {
            Export::Default(local) => self.publish_export(local, "default", None),
            Export::Star {
                module,
                namespace: Some(name),
            } => {
                let id = self.evidence.import_route(module, None);
                self.evidence
                    .forward
                    .insert(name.into(), super::Forward::Namespace(id));
            }
            Export::Star {
                module,
                namespace: None,
            } => self.evidence.stars.push(module.to_owned()),
            Export::Named { module, items } => {
                for Specifier {
                    original, local, ..
                } in items
                {
                    self.publish_export(original, tok(self.tokens, local), module);
                }
            }
        }
    }

    fn publish_export(&mut self, original: usize, public: &str, module: Option<&str>) {
        let name = tok(self.tokens, original);
        let (forward, lookup) = if let Some(module) = module {
            let imp = self.evidence.import_route(module, Some(name));
            (super::Forward::Import(imp), Lookup::Imported(imp))
        } else {
            (
                super::Forward::Local(name.to_owned()),
                Lookup::Name {
                    scope: self.scope,
                    receiver: None,
                },
            )
        };
        self.reference(original, lookup);
        self.evidence.forward.insert(public.to_owned(), forward);
    }
}
