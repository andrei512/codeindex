use super::{
    Binding, Runner, Scopes, Syntax, ident, literal, next_arg,
    wrappers::{WrapperCalls, wrapper_calls},
};
use crate::domain::code::{Language, lexer, script};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    rc::Rc,
};

// Public exports of the literal local modules queried by this source. A cycle
// produces no evidence until an actual imported test registration is found.
pub(super) struct Modules<'a> {
    files: &'a mut ModuleExports,
    cache: HashMap<PathBuf, HashMap<String, Binding>>,
    active: HashSet<PathBuf>,
}

// Written export routes are a property of the pinned file. Final bindings of
// cyclic reexports can depend on earlier siblings in this particular traversal.
#[derive(Default)]
pub(in crate::domain::code::roles) struct ModuleExports {
    files: HashMap<PathBuf, Option<Exports>>,
}

struct Exports {
    path: Rc<Path>,
    routes: HashMap<String, Export>,
}

#[derive(Clone)]
enum Export {
    Known(Binding),
    Follow(Rc<str>, Rc<str>),
}

impl Export {
    fn module(&self) -> Option<&str> {
        match self {
            Self::Follow(spec, _) => Some(spec),
            Self::Known(_) => None,
        }
    }
}

impl Exports {
    fn cached_actions(&self, imports: &[(&str, &str, impl Copy)]) -> Option<Vec<Export>> {
        imports
            .iter()
            .map(|&(_, name, _)| self.routes.get(name).cloned())
            .collect()
    }
}

impl ModuleExports {
    fn group(
        &mut self,
        base: &Path,
        imports: &[(&str, &str, impl Copy)],
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<Option<(Rc<Path>, Vec<Export>)>> {
        match self.files.get(base) {
            Some(None) => return Ok(None),
            Some(Some(exports)) => {
                if let Some(actions) = exports.cached_actions(imports) {
                    return Ok(Some((exports.path.clone(), actions)));
                }
            }
            None => {}
        }
        let Some((path, source)) = read_module(base, read)? else {
            self.files.insert(base.to_owned(), None);
            return Ok(None);
        };
        let exports = self
            .files
            .entry(base.to_owned())
            .or_default()
            .get_or_insert_with(|| Exports {
                path: path.into(),
                routes: HashMap::new(),
            });
        let tokens = lexer::tokens(&source, Language::Js);
        let pairs = script::paired_for_score(&tokens);
        let syntax = Syntax {
            tokens: &tokens,
            pairs: &pairs,
            text: &source,
        };
        let routes = Routes::read(&syntax, imports, |name| !exports.routes.contains_key(name));
        let mut facts = ModuleFacts::default();
        let actions = imports
            .iter()
            .map(|&(_, name, _)| {
                if let Some(action) = exports.routes.get(name) {
                    return action.clone();
                }
                let action = facts.action(&syntax, routes.get(name));
                exports.routes.insert(name.to_owned(), action.clone());
                action
            })
            .collect();
        Ok(Some((exports.path.clone(), actions)))
    }
}

fn module_base(parent: &Path, spec: &str) -> Option<PathBuf> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None;
    }
    crate::domain::code::relative_path(parent.parent()?, spec)
}

fn read_module(
    base: &Path,
    read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
) -> io::Result<Option<(PathBuf, String)>> {
    for path in module_candidates(base) {
        if let Some(source) = read(&path)? {
            return Ok(Some((path, source)));
        }
    }
    Ok(None)
}

fn module_candidates(base: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    let extension = base.extension().and_then(|e| e.to_str());
    let alternatives: &[&str] = match extension {
        Some("js") => &["ts", "tsx", "jsx"],
        Some("mjs") => &["mts"],
        Some("cjs") => &["cts"],
        None => &["ts", "tsx", "js", "jsx"],
        _ => &[],
    };
    std::iter::once(base.to_owned()).chain(alternatives.iter().flat_map(move |ext| {
        std::iter::once_with(move || base.with_extension(ext)).chain(
            extension
                .is_none()
                .then_some(ext)
                .map(|ext| base.join("index").with_extension(ext)),
        )
    }))
}

impl<'a> Modules<'a> {
    pub(super) fn new(files: &'a mut ModuleExports) -> Self {
        Self {
            files,
            cache: HashMap::new(),
            active: HashSet::new(),
        }
    }

    // Imports in one group share a module specifier and retain source order.
    pub(super) fn resolve_imports<Id: Copy>(
        &mut self,
        parent: &Path,
        imports: &[(&str, &str, Id)],
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
        answer: &mut dyn FnMut(&mut Self, Id, Binding),
    ) -> io::Result<()> {
        let spec = imports[0].0;
        let Some(base) = module_base(parent, spec) else {
            let runner = Runner::from_module(spec);
            self.answer_imports(imports, answer, |_, which| runner.export(which));
            return Ok(());
        };
        let cached = self.cache.get(&base);
        if self.active.len() >= 64
            || self.active.contains(&base)
            || cached.is_some_and(|m| imports.iter().all(|(_, name, _)| m.contains_key(*name)))
        {
            self.answer_imports(imports, answer, |modules, which| {
                modules.cached(&base, which).unwrap_or(Binding::Other)
            });
            return Ok(());
        }
        self.cache.entry(base.clone()).or_default();
        let Some(module) = self.files.group(&base, imports, read)? else {
            self.answer_imports(imports, answer, |modules, which| {
                modules.remember(&base, which, Binding::Other)
            });
            return Ok(());
        };
        self.active.insert(base.clone());
        let result = self.resolve_source(&base, module, imports, read, answer);
        self.active.remove(&base);
        result
    }

    fn answer_imports<Id: Copy>(
        &mut self,
        imports: &[(&str, &str, Id)],
        answer: &mut dyn FnMut(&mut Self, Id, Binding),
        mut binding: impl FnMut(&mut Self, &str) -> Binding,
    ) {
        for &(_, which, local) in imports {
            let result = binding(self, which);
            answer(self, local, result);
        }
    }

    fn resolve_source<Id: Copy>(
        &mut self,
        base: &Path,
        (path, mut actions): (Rc<Path>, Vec<Export>),
        imports: &[(&str, &str, Id)],
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
        answer: &mut dyn FnMut(&mut Self, Id, Binding),
    ) -> io::Result<()> {
        let mut onward = Vec::new();
        let mut publish = |modules: &mut Self, index: usize, binding| {
            let (_, which, local) = imports[index];
            let result = modules.remember(base, which, binding);
            answer(modules, local, result);
        };
        for (index, action) in actions.iter_mut().enumerate() {
            self.reuse_export(base, imports[index].1, action);
            let next = action.module();
            if onward
                .last()
                .is_some_and(|&(spec, _, _)| next != Some(spec))
            {
                self.resolve_imports(&path, &onward, read, &mut publish)?;
                onward.clear();
            }
            self.route_export((action, index), &mut onward, &mut publish);
        }
        if !onward.is_empty() {
            self.resolve_imports(&path, &onward, read, &mut publish)?;
        }
        Ok(())
    }

    fn route_export<'e>(
        &mut self,
        (export, index): (&'e Export, usize),
        onward: &mut Vec<(&'e str, &'e str, usize)>,
        publish: &mut impl FnMut(&mut Self, usize, Binding),
    ) {
        match export {
            Export::Known(binding) => publish(self, index, *binding),
            Export::Follow(spec, name) => onward.push((spec.as_ref(), name.as_ref(), index)),
        }
    }

    fn reuse_export(&self, base: &Path, which: &str, export: &mut Export) {
        if let Some(binding) = self.cached(base, which) {
            *export = Export::Known(binding);
        }
    }

    fn cached(&self, base: &Path, which: &str) -> Option<Binding> {
        self.cache.get(base)?.get(which).copied()
    }

    fn remember(&mut self, base: &Path, which: &str, result: Binding) -> Binding {
        let exports = self.cache.get_mut(base).expect("active module has a cache");
        if let Some(&previous) = exports.get(which) {
            return previous;
        }
        exports.insert(which.to_owned(), result);
        result
    }
}

impl<'a> ModuleFacts<'a> {
    fn action(&mut self, s: &'a Syntax, route: Option<Route<'a>>) -> Export {
        let Some(route) = route else {
            return Export::Known(Binding::Other);
        };
        let local = match route {
            Route::Local(local) => local,
            Route::Reexport(spec, name) => return Export::Follow(spec.into(), name.into()),
        };
        let bindings = self
            .scopes
            .get_or_init(|| Scopes::from_names(s, [], true).with_locals());
        if let Some(&(spec, orig, _)) = bindings.relative.iter().find(|(_, _, name)| *name == local)
        {
            return Export::Follow(spec.into(), orig.into());
        }
        let direct = bindings
            .resolve(0, local)
            .binding()
            .unwrap_or(Binding::Other);
        if !matches!(direct, Binding::Other) {
            return Export::Known(direct);
        }
        let flow = self
            .wrappers
            .get_or_insert_with(|| wrapper_calls(s, bindings));
        Export::Known(if flow.reaches(local) {
            Binding::Case
        } else {
            Binding::Other
        })
    }
}

#[derive(Default)]
struct ModuleFacts<'a> {
    scopes: std::cell::OnceCell<Scopes<'a>>,
    wrappers: Option<WrapperCalls<'a>>,
}

#[derive(Clone, Copy)]
enum Route<'a> {
    Local(&'a str),
    Reexport(&'a str, &'a str),
}

// For a single import, avoid a map. For a group, store only the requested
// exports. All names are borrowed until this module's source is released.
enum Routes<'i, 's> {
    One(&'i str, Option<Route<'s>>),
    Many(HashMap<&'i str, Option<Route<'s>>>),
}

impl<'i, 's> Routes<'i, 's> {
    fn read(
        s: &'s Syntax,
        imports: &[(&str, &'i str, impl Copy)],
        missing: impl Fn(&str) -> bool,
    ) -> Self {
        let mut names = imports
            .iter()
            .map(|&(_, name, _)| name)
            .filter(|name| missing(name));
        let first = names.next().expect("group has an uncached export");
        let mut routes = if let Some(second) = names.next() {
            Self::Many(
                [first, second]
                    .into_iter()
                    .chain(names)
                    .map(|name| (name, None))
                    .collect(),
            )
        } else {
            Self::One(first, None)
        };
        let mut remaining = match &routes {
            Self::One(..) => 1,
            Self::Many(m) => m.len(),
        };
        let mut record = |name: &str, route| {
            remaining -= usize::from(routes.record(name, route));
            remaining == 0
        };
        let mut j = 0;
        while j < s.tokens.len() {
            if s.w(j) == "export" && export_at(s, j + 1, &mut record) {
                break;
            }
            j = s.close(j).unwrap_or(j) + 1;
        }
        routes
    }

    // A local export can be superseded. A named re-export finalizes the route.
    fn record(&mut self, name: &str, route: Route<'s>) -> bool {
        let slot = match self {
            Self::One(wanted, slot) if *wanted == name => Some(slot),
            Self::Many(slots) => slots.get_mut(name),
            _ => None,
        };
        let Some(slot) = slot else {
            return false;
        };
        if matches!(slot, Some(Route::Reexport(..))) {
            return false;
        }
        *slot = Some(route);
        matches!(route, Route::Reexport(..))
    }

    fn get(&self, name: &str) -> Option<Route<'s>> {
        match self {
            Self::One(_, value) => *value,
            Self::Many(slots) => slots.get(name).copied().flatten(),
        }
    }
}

fn export_at<'s>(
    s: &'s Syntax,
    head: usize,
    record: &mut impl FnMut(&str, Route<'s>) -> bool,
) -> bool {
    let value = head + 1;
    match s.w(head) {
        "default" if ident(s, value) => {
            let name = if s.w(value) == "function" {
                s.w(value + 1)
            } else {
                s.w(value)
            };
            record("default", Route::Local(name));
        }
        "function" | "const" | "let" => {
            record(s.w(value), Route::Local(s.w(value)));
        }
        "{" => {
            return named_exports(s, head, record);
        }
        _ => {}
    }
    false
}

fn named_exports<'s>(
    syntax: &'s Syntax,
    open: usize,
    mut record: impl FnMut(&str, Route<'s>) -> bool,
) -> bool {
    let Some(end) = syntax.close(open) else {
        return false;
    };
    let from = (syntax.w(end + 1) == "from").then(|| literal(syntax, end + 2));
    let mut pos = open + 1;
    std::iter::from_fn(|| {
        if pos >= end {
            return None;
        }
        let (alias, route) = named_export(syntax, pos, from);
        pos = next_arg(syntax, alias, end) + 1;
        Some((syntax.w(alias), route))
    })
    .any(|(alias, route)| record(alias, route))
}

fn named_export<'s>(syntax: &'s Syntax, pos: usize, from: Option<&'s str>) -> (usize, Route<'s>) {
    let original = syntax.w(pos);
    let alias = pos + usize::from(syntax.w(pos + 1) == "as") * 2;
    let route = from.map_or(Route::Local(original), |spec| {
        Route::Reexport(spec, original)
    });
    (alias, route)
}
