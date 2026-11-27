//! Only prune syntax when captured configuration proves a target predicate
//! false. Features, custom cfg values and an unrecognized target stay unknown.
use super::*;
use std::ops::Range;

#[derive(Clone)]
pub(super) enum Predicate {
    Atom(String, String),
    All(Vec<Predicate>),
    Any(Vec<Predicate>),
    Not(Box<Predicate>),
    Unknown,
}
pub(super) struct Condition {
    span: Range<usize>,
    predicate: Predicate,
}
#[derive(Clone, Copy)]
pub(super) struct Target {
    os: &'static str,
    arch: &'static str,
    family: &'static str,
}
impl Target {
    fn host() -> Self {
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            family: std::env::consts::FAMILY,
        }
    }
    fn triple(v: &str) -> Option<Self> {
        let mut parts = v.split('-');
        let cpu = parts.next()?;
        let os = match (parts.next()?, parts.next()?, parts.next(), parts.next()) {
            ("apple", "darwin", None, None) => "macos",
            ("linux", "android" | "androideabi", None, None)
            | (_, "linux", Some("android" | "androideabi"), None) => "android",
            (_, "linux", Some(_), None) => "linux",
            (_, "windows", Some(_), None) => "windows",
            (_, "freebsd", None, None) => "freebsd",
            _ => return None,
        };
        const ARCHITECTURES: &[(&str, &str)] = &[
            ("aarch64", "aarch64"),
            ("x86_64", "x86_64"),
            ("i686", "x86"),
            ("i586", "x86"),
        ];
        let arch = ARCHITECTURES.iter()
            .find_map(|(spelling, arch)| (*spelling == cpu).then_some(*arch))
            .unwrap_or("");
        Some(Self {
            os,
            arch,
            family: if os == "windows" { "windows" } else { "unix" },
        })
    }
    fn matches(target: Option<Self>, key: &str, value: &str) -> Option<bool> {
        match key {
            "true" => Some(true),
            "false" => Some(false),
            "unix" | "windows" => Some(target?.family == key),
            "target_family" => Some(target?.family == value),
            "target_os" => Some(target?.os == value),
            "target_arch" => {
                let arch = target?.arch;
                (!arch.is_empty()).then_some(arch == value)
            }
            _ => None,
        }
    }
    fn config(s: &str) -> Option<Option<Self>> {
        let mut build = false;
        for raw in super::cargo::manifest_lines(s) {
            match super::cargo::ManifestLine::parse(raw) {
                super::cargo::ManifestLine::Section(section) => build = section == "[build]",
                super::cargo::ManifestLine::Entry("target", value) if build => {
                    let text = value.trim().trim_matches(&['\'', '"'][..]);
                    return Some(Self::triple(text));
                }
                _ => {}
            }
        }
        None
    }
}
impl Predicate {
    // cfg_attr(g, cfg(p)) imposes the implication g -> p.
    fn guarded_by(self, guards: &[Predicate]) -> Self {
        if guards.is_empty() {
            return self;
        }
        Predicate::Any(vec![
            Predicate::Not(Box::new(Predicate::All(guards.to_vec()))),
            self,
        ])
    }
    pub(super) fn eval(&self, target: Option<Target>) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::Atom(key, value) => Target::matches(target, key, value),
            Self::Not(x) => Some(!x.eval(target)?),
            Self::All(xs) | Self::Any(xs) => {
                let all = matches!(self, Self::All(_));
                Self::combine(xs, target, all)
            }
        }
    }
    pub(super) fn combine(terms: &[Self], target: Option<Target>, all: bool) -> Option<bool> {
        let mut result = Some(all);
        for term in terms {
            let value = term.eval(target);
            if value == Some(!all) {
                return value;
            }
            result = result.and(value);
        }
        result
    }
}
/// Attribute argument lists and boolean predicates use the same balanced
/// meta-item boundaries, including commas inside nested calls.
#[derive(Clone, Copy)]
struct Meta<'a, 's> {
    tokens: &'a [Token<'s>],
    pairs: &'a [usize],
    start: usize,
    end: usize,
}
impl<'a, 's> Meta<'a, 's> {
    fn args(self) -> Option<impl Iterator<Item = Self>> {
        let open = self.start + 1;
        let close = self.end.checked_sub(1)?;
        let paired = self.tokens.get(open).filter(|token| token.text == "(")
            .and_then(|_| self.pairs.get(open)).copied();
        if open >= close || paired != Some(close) {
            return None;
        }
        let mut start = open + 1;
        Some(std::iter::from_fn(move || {
            if start >= close {
                return None;
            }
            let end = until_before(self.tokens, self.pairs, start..close, &[","]);
            let arg = Self { start, end, ..self };
            start = end + 1;
            Some(arg)
        }))
    }
    fn first_arg(self) -> Option<(Self, impl Iterator<Item = Self>)> {
        let mut args = self.args()?;
        Some((args.next()?, args))
    }
    fn value(self) -> Option<&'s str> {
        match &self.tokens[self.start..self.end] {
            [_, eq, value] if eq.text == "=" => Some(value.text),
            _ => None,
        }
    }
    fn predicate(self, depth: usize) -> Predicate {
        let name = word(self.tokens, self.start);
        if self.end == self.start + 1 {
            return Predicate::Atom(name.into(), String::new());
        }
        if let Some(value) = self.value().and_then(path_string) {
            return Predicate::Atom(name.into(), value.into());
        }
        self.group_predicate(name, depth).unwrap_or(Predicate::Unknown)
    }

    fn group_predicate(self, name: &str, depth: usize) -> Option<Predicate> {
        if depth >= 32 {
            return None;
        }
        let mut terms = self.args()?.map(|arg| arg.predicate(depth + 1));
        match name {
            "all" => Some(Predicate::All(terms.collect())),
            "any" => Some(Predicate::Any(terms.collect())),
            "not" => {
                let term = terms.next()?;
                terms.next().is_none().then_some(Predicate::Not(Box::new(term)))
            }
            _ => None,
        }
    }
}
/// Consume consecutive outer attributes once, before visibility or the item.
pub(super) fn attributes(
    t: &[Token],
    pairs: &[usize],
    mut at: usize,
    conditions: &mut Vec<Condition>,
) -> (usize, Option<(usize, ModulePaths)>) {
    let mut attrs = OuterAttributes::default();
    while word(t, at) == "#" && word(t, at + 1) == "[" {
        let Some(after) = after_group(pairs, at + 1) else {
            return (t.len(), None);
        };
        attrs.observe(Meta {
            tokens: t,
            pairs,
            start: at + 2,
            end: after - 1,
        });
        at = after;
    }
    if !attrs.predicates.is_empty()
        && let Some(span) = item_span(t, pairs, at)
    {
        conditions.extend(attrs.predicates.into_iter().map(|predicate| Condition {
            span: span.clone(),
            predicate,
        }));
    }
    let paths = attrs.paths;
    if paths.is_empty() {
        return (at, None);
    }
    let keyword = after_visibility(t, pairs, at);
    let module = (word(t, keyword) == "mod").then_some((keyword, paths));
    (at, module)
}

fn after_visibility(tokens: &[Token], pairs: &[usize], at: usize) -> usize {
    if word(tokens, at) != "pub" {
        return at;
    }
    let next = at + 1;
    if word(tokens, next) == "(" {
        return after_group(pairs, next).unwrap_or(tokens.len());
    }
    next
}

#[derive(Default)]
struct OuterAttributes {
    guards: Vec<Predicate>,
    paths: Vec<(Vec<Predicate>, String)>,
    predicates: Vec<Predicate>,
}

enum Attribute {
    Path,
    Predicate,
    Guarded,
}

impl Attribute {
    fn read(meta: Meta) -> Option<Self> {
        match word(meta.tokens, meta.start) {
            "path" => Some(Self::Path),
            "cfg" => Some(Self::Predicate),
            "cfg_attr" => Some(Self::Guarded),
            _ => None,
        }
    }
}

impl OuterAttributes {
    fn observe(&mut self, meta: Meta) {
        match Attribute::read(meta) {
            Some(Attribute::Path) => {
                let path = meta.value().and_then(path_string).unwrap_or("");
                self.paths.push((self.guards.clone(), path.into()));
            }
            Some(Attribute::Predicate) => self.observe_predicate(meta),
            Some(Attribute::Guarded) => self.observe_guarded(meta),
            None => {}
        }
    }

    fn observe_predicate(&mut self, meta: Meta) {
        let Some((first, mut rest)) = meta.first_arg() else {
            return;
        };
        if rest.next().is_none() {
            self.predicates
                .push(first.predicate(0).guarded_by(&self.guards));
        }
    }

    fn observe_guarded(&mut self, meta: Meta) {
        let Some((first, rest)) = meta.first_arg() else {
            return;
        };
        if self.guards.len() >= 32 {
            self.paths.push((self.guards.clone(), String::new()));
            return;
        }
        self.guards.push(first.predicate(0));
        for nested in rest {
            self.observe(nested);
        }
        self.guards.pop();
    }
}

fn path_string(raw: &str) -> Option<&str> {
    if let Some(raw) = raw.strip_prefix('r') {
        let hashes = raw.find('"')?;
        let (tag, rest) = raw.split_at(hashes);
        if !tag.bytes().all(|b| b == b'#') {
            return None;
        }
        return rest.strip_suffix(tag)?.strip_prefix('"')?.strip_suffix('"');
    }
    raw.strip_prefix('"')?
        .strip_suffix('"')
        .filter(|s| !s.contains('\\'))
}

fn item_span(t: &[Token], pairs: &[usize], item: usize) -> Option<Range<usize>> {
    let start = t.get(item)?.span.start;
    let delimiter = until(t, pairs, item, &["{", ";", ",", "}"]);
    let finish = if word(t, delimiter) == "{" && pairs[delimiter] != usize::MAX {
        pairs[delimiter]
    } else {
        delimiter.min(t.len() - 1)
    };
    Some(start..t[finish].span.end)
}
fn cargo_config<'a>(unit: &Unit<'a>) -> Option<(&'a Path, usize)> {
    let path = Path::new(unit.path);
    let dir = path.parent()?;
    if dir.file_name()? != ".cargo" {
        return None;
    }
    let priority = match path.file_name()?.to_str()? {
        "config" => 2,
        "config.toml" => 1,
        _ => return None,
    };
    Some((dir.parent()?, priority))
}

#[derive(Default)]
pub(super) struct FileConfig {
    pub ranges: Vec<Range<usize>>,
    // Empty when every declaration is active. Only configured files need a bitmap.
    active_declarations: Vec<bool>,
    pub target: Option<Target>,
}

impl FileConfig {
    fn new(
        target: Option<Target>,
        conditions: &[Condition],
        declarations: &[super::Declaration],
    ) -> Self {
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for span in conditions
            .iter()
            .filter(|c| c.predicate.eval(target) == Some(false))
            .map(|c| &c.span)
        {
            extend_inactive_ranges(&mut ranges, span);
        }
        let active_declarations = if ranges.is_empty() {
            Vec::new()
        } else {
            declarations
                .iter()
                .map(|d| active(&ranges, d.span.start))
                .collect()
        };
        Self {
            target,
            ranges,
            active_declarations,
        }
    }
    pub(super) fn declaration_active(&self, local: usize) -> bool {
        self.active_declarations.get(local).copied().unwrap_or(true)
    }
}

fn extend_inactive_ranges(ranges: &mut Vec<Range<usize>>, span: &Range<usize>) {
    if let Some(last) = ranges.last_mut().filter(|last| last.end >= span.start) {
        last.end = last.end.max(span.end);
    } else {
        ranges.push(span.clone());
    }
}

pub(super) fn file_configs(units: &[Unit]) -> Vec<FileConfig> {
    let mut configs = HashMap::new();
    let candidates = units
        .iter()
        .filter_map(|unit| cargo_config(unit).map(|(base, priority)| (base, priority, unit.text)));
    for (base, priority, text) in candidates {
        select_config(configs.entry(base).or_insert((0, None)), priority, text);
    }
    let mut targets = HashMap::new();
    units
        .iter()
        .map(|u| {
            let Some(evidence) = u.analysis.rust.as_ref().filter(|evidence| needs_target(evidence)) else {
                return FileConfig::default();
            };
            let file = Path::new(u.path);
            let dir = file.parent().unwrap_or(file);
            let target = crate::domain::code::nearest_owner(&mut targets, dir, |p| {
                configs.get(p).and_then(|(_, t)| *t)
            })
            .unwrap_or_else(|| Some(Target::host()));
            FileConfig::new(target, &evidence.conditions, &u.analysis.declarations)
        })
        .collect()
}
fn select_config(selected: &mut (usize, Option<Option<Target>>), priority: usize, text: &str) {
    if priority > selected.0 {
        *selected = (priority, Target::config(text));
    }
}

fn needs_target(evidence: &Evidence) -> bool {
    !evidence.conditions.is_empty() || !evidence.scopes[0].children.is_empty()
}

pub(super) fn active(ranges: &[Range<usize>], at: usize) -> bool {
    !ranges
        .get(ranges.partition_point(|r| r.end <= at))
        .is_some_and(|r| r.start <= at)
}

crate::domain::code::cache::record!(Condition { span, predicate });

crate::domain::code::cache::tagged!(Predicate {
    0 => Atom(key, value),
    1 => All(terms),
    2 => Any(terms),
    3 => Not(term),
    4 => Unknown,
});
