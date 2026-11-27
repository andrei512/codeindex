//! Written generic arguments used only when the path identifies a standard
//! collection or pointer. A same-named repository type keeps its own members.
use super::*;
#[derive(Clone)]
pub(super) struct Argument {
    pub outer: Vec<String>,
    pub inner: Vec<String>,
    pub index: usize,
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Projection {
    Value,
    Element,
    Unwrap,
    Tuple(usize),
}
impl Projection {
    /// One projection may be propagated across an initializer. Composing two
    /// nonidentity projections would require type information we do not retain.
    pub(super) fn through(self, initializer: Self) -> Option<Self> {
        match (self, initializer) {
            (Self::Value, next) | (next, Self::Value) => Some(next),
            _ => None,
        }
    }
}

enum WrittenArguments<'a> {
    Tuple,
    Array,
    Generic(&'a [String]),
}

impl<'a> WrittenArguments<'a> {
    fn read(t: &[Token], written: std::ops::Range<usize>, path: &'a [String]) -> Option<(Self, usize)> {
        let start = written.start;
        match word(t, start) {
            "(" => Some((Self::Tuple, start)),
            "[" => Some((Self::Array, start)),
            _ if accepts_generic(t, written.end, path) => Some((Self::Generic(path), written.end)),
            _ => None,
        }
    }

    fn outer(&self) -> Vec<String> {
        match self {
            Self::Tuple => vec!["()".to_owned()],
            Self::Array => vec!["[]".to_owned()],
            Self::Generic(path) => path.to_vec(),
        }
    }
}

fn accepts_generic(tokens: &[Token], open: usize, path: &[String]) -> bool {
    !path.is_empty() && word(tokens, open) == "<"
        && (path.len() == 1 || standard_projection(path).is_some())
}

pub(super) fn arguments(
    t: &[Token],
    written: std::ops::Range<usize>,
    path: &[String],
) -> Vec<Argument> {
    let Some((shape, open)) = WrittenArguments::read(t, written, path) else { return Vec::new() };
    let slots = std::iter::successors(Some(open + 1), |&at| {
        if matches!(shape, WrittenArguments::Tuple) {
            return next_tuple_field(t, at);
        }
        None
    });
    slots.take_while(|&at| at < t.len()).enumerate().filter_map(|(index, at)| {
        let inner = members::path(t, at);
        (!inner.is_empty()).then(|| Argument { inner, index, outer: shape.outer() })
    }).collect()
}

fn next_tuple_field(t: &[Token], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (at, token) in t.iter().enumerate().skip(start) {
        match (token.text, depth) {
            (",", 0) => return Some(at + 1),
            (";", 0) | ("{" | "}", _) => return None,
            ("<" | "(" | "[", _) => depth += 1,
            (">" | ")" | "]", _) => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    None
}

fn standard_projection(path: &[String]) -> Option<Projection> {
    let [root, module, name] = path else { return None };
    match (root.as_str(), module.as_str(), name.as_str()) {
        ("std" | "alloc", "sync", "Arc") | ("std" | "alloc", "rc", "Rc")
        | ("std" | "alloc", "boxed", "Box") => Some(Projection::Value),
        ("std" | "alloc", "vec", "Vec")
        | ("std" | "alloc", "collections", "VecDeque") => Some(Projection::Element),
        ("std" | "core", "option", "Option") | ("std" | "core", "result", "Result") =>
            Some(Projection::Unwrap),
        _ => None,
    }
}

/// Written arguments are emitted in increasing slot order. An unrecognized
/// tuple field leaves a gap, so its slot can differ from its vector position.
pub(super) fn selected<'a>(
    r: &Resolver, at: Address, args: &'a [Argument], projection: Projection,
) -> Option<&'a [String]> {
    if let Projection::Tuple(index) = projection {
        let slot = args.binary_search_by_key(&index, |arg| arg.index).ok()?;
        let arg = &args[slot];
        return (arg.outer.last()?.as_str() == "()").then_some(&arg.inner);
    }
    let arg = args.first()?;
    (arg.index == 0 && permits(r, at, arg, projection)).then_some(&arg.inner)
}

fn permits(r: &Resolver, at: Address, a: &Argument, p: Projection) -> bool {
    let last = a.outer.last().map(String::as_str).unwrap_or("");
    if last == "[]" {
        return p == Projection::Element;
    }
    if a.outer.len() != 1 {
        return standard_projection(&a.outer) == Some(p);
    }
    let scopes = &r.evidence(at.0).scopes;
    let mut ancestry = std::iter::successors(Some(at.1), |&id| {
        (id != scopes[id].module)
            .then_some(scopes[id].parent)
            .flatten()
    });
    let binding = ancestry.find_map(|id| {
        if scopes[id].names.contains_key(last) {
            return Some(None);
        }
        r.alias(at.0, id, last).map(Some)
    });
    if let Some(alias) = binding {
        return alias.and_then(|path| standard_projection(path)) == Some(p);
    }
    // Only names in the standard prelude may resolve without a use item.
    matches!(
        (p, last),
        (Projection::Value, "Box")
            | (Projection::Element, "Vec")
            | (Projection::Unwrap, "Result" | "Option")
    )
}

crate::domain::code::cache::record!(Argument {
    outer,
    inner,
    index
});

crate::domain::code::cache::tagged!(Projection {
    0 => Value,
    1 => Element,
    2 => Unwrap,
    3 => Tuple(index),
});
