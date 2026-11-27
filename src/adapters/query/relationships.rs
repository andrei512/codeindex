mod hits;
mod position;
mod selection;
use super::{
    Pin,
    json::{ObjectArray, array, object, quote},
};
use crate::domain::{
    code::{
        Language,
        navigation::{Coverage, Navigation},
    },
    content_lookup::ExtentRead,
};
use crate::{
    adapters::snapshot::manifest::{REL, SYMBOLS},
    application::index::{
        graph::{Decl, DeclRef}, relationship_lookup::Relations, symbol_lookup::SymbolLookup,
    },
};
use std::{
    borrow::Cow,
    cell::{LazyCell, OnceCell},
    collections::BTreeMap,
    io,
    path::{Component, Path},
    rc::Rc,
};
pub(super) fn relative(root: &Path, cwd: &Path, input: &str) -> io::Result<String> {
    let path = cwd.join(input);
    let mut out = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            _ => out.push(c.as_os_str()),
        }
    }
    out.strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("file outside worktree"))
}
fn generated_identity(pin: &Pin, d: &Decl) -> String {
    let path = pin.files[d.file].path();
    let Some(engine) = Language::for_path(path).and_then(|lang| lang.navigation(path).engine)
    else {
        return format!("native {path} {} {}", d.span.start, d.name);
    };
    let navigation_only = matches!(
        d.kind.as_str(),
        "local_binding" | "parameter" | "shell_assignment" | "field_declaration" | "css_variable"
    );
    if navigation_only {
        format!("{engine} {path} {}", d.span.start)
    } else {
        format!("{engine} {path} {} {}", d.span.start, d.name)
    }
}
struct CachedDecl {
    decl: Decl,
    label: OnceCell<Rc<str>>,
    caller: OnceCell<Rc<str>>,
}

impl CachedDecl {
    fn new(decl: Decl) -> Self {
        Self {
            decl,
            label: OnceCell::new(),
            caller: OnceCell::new(),
        }
    }

    fn label(&self, pin: &Pin) -> &Rc<str> {
        self.label.get_or_init(|| match self.decl.label.as_deref() {
            Some(label) => Rc::from(label),
            None => generated_identity(pin, &self.decl).into(),
        })
    }

    fn caller(&self) -> Rc<str> {
        Rc::clone(self.caller.get_or_init(|| self.decl.name.as_str().into()))
    }
}

/// Semantic declarations and their display identities are decoded on demand.
struct QuerySymbols<'a, R> {
    pin: &'a Pin,
    index: SymbolLookup<R>,
    cached: BTreeMap<usize, CachedDecl>,
}

impl<R: ExtentRead> QuerySymbols<'_, R> {
    fn get(&mut self, id: usize) -> io::Result<&CachedDecl> {
        use std::collections::btree_map::Entry;
        Ok(match self.cached.entry(id) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                entry.insert(CachedDecl::new(self.index.semantic_declaration(id as u64)?))
            }
        })
    }

    fn collect(
        &mut self,
        graph: &mut Relations<impl ExtentRead>,
        op: Operation,
        selection: &selection::Selection,
        compact: bool,
    ) -> io::Result<hits::Hits> {
        let mut hits = hits::Hits::default();
        let shape = HitShape::new(op, selection, compact);
        for &id in &selection.targets {
            match &shape {
                HitShape::Declaration { evidence } => self.definition_hits(id, selection, *evidence, &mut hits)?,
                HitShape::Incoming(incoming) => incoming.collect(id, graph, self, &mut hits)?,
            }
        }
        Ok(hits)
    }

    fn definition_hits(
        &mut self,
        id: usize,
        selection: &selection::Selection,
        include_evidence: bool,
        hits: &mut hits::Hits,
    ) -> io::Result<()> {
        let pin = self.pin;
        let decl = self.get(id)?;
        let mut contexts = selection.definition_contexts(id);
        let Some(first) = contexts.next() else { return Ok(()) };
        let label = decl.label(pin);
        let key = (decl.decl.file, decl.decl.span.start, Some(label.clone()), None);
        hits.save_row(key, |evidence| {
            if include_evidence {
                evidence.extend(std::iter::once(first).chain(contexts)
                    .map(|context| (context, label.clone())));
            }
        });
        Ok(())
    }

    fn context_json(&self, context_id: &str, roles: selection::Roles) -> String {
        let bindings = array(roles.into_iter().map(|(role, ids)| {
            let mut labels: Vec<_> =
                ids.into_iter().map(|id| self.cached[&id].label(self.pin)).collect();
            labels.sort_unstable();
            labels.dedup();
            object(&[
                ("role", quote(role)),
                (
                    "certainty",
                    quote(if labels.is_empty() {
                        "unresolved"
                    } else {
                        "candidates"
                    }),
                ),
                ("targets", array(labels.iter().map(|s| quote(s)))),
            ])
        }));
        object(&[
            ("context", quote(context_id)),
            ("coverage", quote("partial")),
            ("reason", quote("unverified_context")),
            ("bindings", bindings),
        ])
    }

    fn caller_labels(&mut self, owner: Option<usize>) -> io::Result<(Option<Rc<str>>, Option<Rc<str>>)> {
        let Some(owner) = owner else { return Ok((None, None)); };
        let pin = self.pin;
        let decl = self.get(owner)?;
        Ok((Some(decl.label(pin).clone()), Some(decl.caller())))
    }
}

enum HitShape<'a> {
    Declaration { evidence: bool },
    Incoming(Incoming<'a>),
}

impl<'a> HitShape<'a> {
    fn new(operation: Operation, selection: &'a selection::Selection, compact: bool) -> Self {
        let kind = match operation {
            Operation::Definition => return Self::Declaration { evidence: !compact },
            Operation::References => SiteKind::Reference,
            Operation::Callers => SiteKind::Call,
        };
        let target = match (compact, kind) {
            (false, _) => TargetLabel::Evidence,
            (true, SiteKind::Reference) if selection.targets.len() > 1 => TargetLabel::Identity,
            (true, _) => TargetLabel::None,
        };
        Self::Incoming(Incoming { selection, kind, target })
    }
}

#[derive(Clone, Copy)]
enum SiteKind {
    Reference,
    Call,
}

impl SiteKind {
    fn accepts(self, site: &crate::application::index::graph::UseLocation) -> bool {
        match self {
            Self::Reference => true,
            Self::Call => site.is_call,
        }
    }
}

enum TargetLabel {
    Evidence,
    Identity,
    None,
}

impl TargetLabel {
    fn resolve(&self, fetch: impl FnOnce() -> io::Result<Rc<str>>) -> io::Result<Option<Rc<str>>> {
        match self {
            Self::None => Ok(None),
            Self::Evidence | Self::Identity => fetch().map(Some),
        }
    }

    fn evidence<'a>(&self, label: Option<&'a Rc<str>>) -> Option<&'a Rc<str>> {
        match self {
            Self::Evidence => label,
            _ => None,
        }
    }
}

struct Incoming<'a> {
    selection: &'a selection::Selection,
    kind: SiteKind,
    target: TargetLabel,
}

impl Incoming<'_> {
    fn accepts(&self, id: usize, site: &crate::application::index::graph::UseLocation) -> bool {
        self.kind.accepts(site) && self.selection.accepts(id, site.context)
    }

    fn collect(
        &self,
        id: usize,
        graph: &mut Relations<impl ExtentRead>,
        symbols: &mut QuerySymbols<impl ExtentRead>,
        hits: &mut hits::Hits,
    ) -> io::Result<()> {
        let pin = symbols.pin;
        let label = self.target.resolve(|| symbols.get(id).map(|decl| decl.label(pin).clone()))?;
        let evidence = self.target.evidence(label.as_ref());
        graph.visit_incoming(id, |site| {
            if !self.accepts(id, &site) || !hits.accepts_location(site.file, site.span.start) {
                return Ok(());
            }
            let (symbol, caller) = match self.kind {
                SiteKind::Reference => (label.clone(), None),
                SiteKind::Call => symbols.caller_labels(site.owner)?,
            };
            hits.save((site.file, site.span.start, symbol, caller), site.context, evidence);
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
enum Subject<'a> {
    Name(&'a str),
    Position(&'a str, position::Position),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    Definition,
    References,
    Callers,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::References => "references",
            Self::Callers => "callers",
        }
    }
}

pub struct Query<'a> {
    operation: Operation,
    subject: Subject<'a>,
}

impl<'a> Query<'a> {
    pub(super) fn parse(operation: Operation, args: &'a [String]) -> Result<Self, String> {
        Self::parse_args(operation, args, position::Encoding::Utf16).map(|(query, _)| query)
    }

    pub(super) fn parse_public(
        operation: Operation,
        args: &'a [String],
    ) -> Result<(Self, &'a [String]), String> {
        Self::parse_args(operation, args, position::Encoding::Bytes)
    }

    fn parse_args(
        operation: Operation,
        args: &'a [String],
        encoding: position::Encoding,
    ) -> Result<(Self, &'a [String]), String> {
        let (operands, flags) = match encoding {
            position::Encoding::Bytes => Self::public_operands(args)?,
            position::Encoding::Utf16 => (args, &[][..]),
        };
        let subject = match operands {
            [path, line, col] => Subject::Position(path, position::Position::parse(line, col, encoding)?),
            [name] if operation != Operation::Definition && !name.is_empty() => Subject::Name(name),
            _ => return Err("invalid query arguments".into()),
        };
        Ok((Self { operation, subject }, flags))
    }

    fn public_operands(args: &'a [String]) -> Result<(&'a [String], &'a [String]), String> {
        let count = if args.get(1).is_some_and(|line| !line.starts_with('-')) {
            3
        } else {
            1
        };
        args.split_at_checked(count)
            .ok_or_else(|| "missing indexed query operand".into())
    }

    pub(super) fn run(
        self,
        pin: &Pin,
        cwd: &Path,
        display: &super::Display,
        outcome: &mut super::Outcome,
    ) -> io::Result<String> {
        let Self {
            operation: op,
            subject,
        } = self;
        let compact = display.compact();
        let mut symbols = QuerySymbols {
            pin,
            index: SymbolLookup::open(pin.reader(SYMBOLS)?, pin.files.len())?,
            cached: BTreeMap::new(),
        };
        let reply = Reply {
            pin,
            cwd,
            display,
            op,
        };
        let document = match symbols.locate(subject, cwd) {
            Ok(doc) => doc,
            Err(error) => return reply.location_error(subject, error, outcome),
        };
        let mut cursor = CursorEvidence::new(display);
        let profile = symbols.profile(&document, &mut cursor)?;
        let file = match document.indexed_file(pin, op, profile) {
            Ok(file) => file,
            Err(unavailable) => {
                return Ok(reply.rejected(
                    Rejection::Unavailable { path: &document.path, profile, reason: unavailable },
                    outcome,
                ));
            }
        };
        let mut graph = Relations::open(pin.reader(REL)?, pin.files.len(), symbols.index.count())?;
        let selection = symbols.select(document.focus, file, &mut graph, cursor)?;
        let selected = symbols.collect(&mut graph, op, &selection, compact)?;
        if display.public() {
            outcome.code = u8::from(selected.rows.is_empty());
        }
        if let Some(context) = display.context() {
            return reply.compact(context, selected, selection.targets.is_empty(), outcome);
        }
        let keys = selection
            .contexts
            .keys()
            .map(|&id| Ok((id, graph.context_key(id)?)))
            .collect::<io::Result<BTreeMap<_, _>>>()?;
        reply.structured(
            ContextKeys { by_id: keys, fallback: LazyCell::new(|| context_key(pin, &document.path, profile)) },
            selection,
            selected,
            &symbols,
        )
    }
}

#[derive(Clone, Copy)]
enum Focus {
    Declaration(usize),
    Cursor(position::Position),
}

struct Document<'p> {
    path: Cow<'p, str>,
    file: Option<usize>,
    focus: Focus,
}

enum Unavailable {
    Unindexed,
    UnsupportedUnindexed,
    UnsupportedIndexed,
}

impl Unavailable {
    fn indexed(&self) -> bool {
        matches!(self, Self::UnsupportedIndexed)
    }

    fn status(&self) -> &'static str {
        match self {
            Self::Unindexed => "unindexed",
            _ => "unsupported",
        }
    }

    fn diagnostic(&self) -> &'static str {
        match self {
            Self::UnsupportedIndexed => "cx: unsupported operation\n",
            _ => "cx: unindexed document\n",
        }
    }
}

impl Document<'_> {
    fn indexed_file(
        &self,
        pin: &Pin,
        op: Operation,
        profile: Option<Navigation>,
    ) -> Result<usize, Unavailable> {
        let coverage = profile.map(|p| p.coverage);
        let file = self.file.filter(|&i| {
            pin.files[i].bytes.is_some() && coverage.is_some_and(|c| c != Coverage::Unavailable)
        });
        let unsupported =
            op == Operation::Callers && coverage.is_some_and(|c| c != Coverage::Calls);
        match (file, unsupported) {
            (Some(id), false) => Ok(id),
            (None, false) => Err(Unavailable::Unindexed),
            (None, true) => Err(Unavailable::UnsupportedUnindexed),
            (Some(_), true) => Err(Unavailable::UnsupportedIndexed),
        }
    }
}

/// Some dialects identify their navigation capabilities from declarations.
struct CursorEvidence {
    selection: selection::Selection,
    offset: Option<io::Result<usize>>,
    visited: bool,
}

impl CursorEvidence {
    fn new(display: &super::Display) -> Self {
        Self {
            selection: selection::Selection::new(!display.compact()),
            offset: None,
            visited: false,
        }
    }

    /// Availability errors take precedence over a bad column. Resolve the
    /// position only when a declaration could contribute to the answer.
    fn observe(&mut self, id: usize, d: DeclRef<'_>, offset: impl FnOnce() -> io::Result<usize>) {
        if d.kind == "source_file" {
            return;
        }
        if let Ok(&at) = self.offset.get_or_insert_with(offset).as_ref() {
            self.declaration(at, id, d);
        }
    }

    fn declaration(&mut self, at: usize, id: usize, d: DeclRef<'_>) {
        if d.kind != "source_file" && d.span.contains(&at) {
            self.selection.consider(d.span.len(), &[id], "definition");
        }
    }
}

impl<'p, R: ExtentRead> QuerySymbols<'p, R> {
    fn locate(&mut self, subject: Subject<'_>, cwd: &Path) -> io::Result<Document<'p>> {
        match subject {
            Subject::Name(name) => {
                let (id, decl) = self.index.named_target(name)?;
                let file = decl.file;
                self.cached.insert(id, CachedDecl::new(decl));
                Ok(Document {
                    path: Cow::Borrowed(self.pin.files[file].path()),
                    file: Some(file),
                    focus: Focus::Declaration(id),
                })
            }
            Subject::Position(input, position) => {
                let path = relative(&self.pin.root, cwd, input)?;
                let file = self
                    .pin
                    .files
                    .binary_search_by(|f| f.path().cmp(&path))
                    .ok();
                Ok(Document {
                    path: Cow::Owned(path),
                    file,
                    focus: Focus::Cursor(position),
                })
            }
        }
    }
    fn profile(
        &mut self,
        document: &Document,
        cursor: &mut CursorEvidence,
    ) -> io::Result<Option<Navigation>> {
        let Some(language) = Language::for_path(&document.path) else {
            return Ok(None);
        };
        language
            .document_navigation(&document.path, |accepts| {
                if let Focus::Declaration(id) = document.focus
                    && accepts(&self.get(id)?.decl.kind)
                {
                    return Ok(true);
                }
                let Some(file) = document.file else { return Ok(false) };
                let mut found = false;
                self.index.visit_file(file, |id, d| {
                    found |= accepts(d.kind);
                    if let Focus::Cursor(position) = document.focus {
                        cursor.observe(id, d, || position.offset(self.pin, file));
                    }
                })?;
                cursor.visited = true;
                Ok(found)
            })
            .map(Some)
    }
    fn select(
        &mut self,
        focus: Focus,
        file: usize,
        graph: &mut Relations<impl ExtentRead>,
        mut cursor: CursorEvidence,
    ) -> io::Result<selection::Selection> {
        match focus {
            Focus::Cursor(position) => {
                let at = cursor.offset.take().unwrap_or_else(|| {
                    position.offset(self.pin, file)
                })?;
                if !cursor.visited {
                    self.index.visit_file(file, |id, d| cursor.declaration(at, id, d))?;
                }
                graph.visit_position(file, at, |u, ids| {
                    cursor.selection.scoped(
                        u.span.len(),
                        ids,
                        if u.is_call {
                            "call_target"
                        } else {
                            "reference"
                        },
                        u.context,
                    )
                })?;
            }
            Focus::Declaration(id) => cursor.selection.consider(0, &[id], "definition"),
        }
        cursor.selection.attach_definition_contexts(focus, graph)?;
        Ok(cursor.selection)
    }
}

impl selection::Selection {
    fn attach_definition_contexts(
        &mut self,
        focus: Focus,
        graph: &mut Relations<impl ExtentRead>,
    ) -> io::Result<()> {
        if !graph.contextual() {
            return Ok(());
        }
        let mut add = |id| -> io::Result<()> {
            for ctx in graph.definition_contexts(id)? {
                self.contexts
                    .entry(ctx)
                    .or_default()
                    .entry("definition")
                    .or_default()
                    .insert(id);
            }
            Ok(())
        };
        match focus {
            Focus::Declaration(id) => add(id)?,
            Focus::Cursor(_) => {
                if let Some(ids) = self
                    .roles
                    .as_ref()
                    .and_then(|roles| roles.get("definition"))
                {
                    for &id in ids {
                        add(id)?;
                    }
                }
            }
        }
        Ok(())
    }
}

enum Rejection<'a> {
    UnresolvedName(&'a str),
    Unavailable {
        path: &'a str,
        profile: Option<Navigation>,
        reason: Unavailable,
    },
}

struct Reply<'a> {
    pin: &'a Pin,
    cwd: &'a Path,
    display: &'a super::Display,
    op: Operation,
}

impl Reply<'_> {
    fn response(&self, indexed: bool, status: &str, contexts: String, page: JsonPage) -> String {
        let Self { pin, op, .. } = *self;
        object(&[
            ("root", quote(&pin.root.to_string_lossy())),
            ("mode", quote(op.name())),
            ("generation", pin.generation.to_string()),
            ("reused", pin.reused.to_string()),
            ("indexed_document", indexed.to_string()),
            (
                "engine",
                if indexed {
                    quote("codeindex")
                } else {
                    "null".into()
                },
            ),
            ("operation_status", quote(status)),
            ("relationship_status", quote("partial")),
            ("position_encoding", quote("one-based UTF-16")),
            ("contexts", contexts),
            ("result", page.hits.finish()),
            ("evidence", page.evidence.finish()),
            ("truncated", page.truncated.to_string()),
        ])
    }

    fn location_error(
        &self,
        subject: Subject<'_>,
        error: io::Error,
        outcome: &mut super::Outcome,
    ) -> io::Result<String> {
        let Subject::Name(name) = subject else {
            return Err(error);
        };
        match error.kind() {
            io::ErrorKind::NotFound if self.display.public() => {
                Ok(self.rejected(Rejection::UnresolvedName(name), outcome))
            }
            io::ErrorKind::InvalidInput => {
                let advice = self.position_advice(name.rsplit("::").next().unwrap_or(name));
                Err(io::Error::new(error.kind(), format!("{error}; use {advice}")))
            }
            _ => Err(error),
        }
    }
    fn position_advice(&self, symbol: &str) -> String {
        if self.display.public() {
            format!("cx --symbol {symbol} --column and pass the selected path, line and byte column to cx --reference from the repository root")
        } else {
            format!("cx --symbol {symbol} --json and pass the selected path, line and col to cx --cx {} from the repository root", self.op.name())
        }
    }

    fn rejected(&self, rejection: Rejection<'_>, outcome: &mut super::Outcome) -> String {
        match rejection {
            Rejection::UnresolvedName(name) => self.unresolved_name(name, outcome),
            Rejection::Unavailable { path, profile, reason } => {
                self.unavailable(path, profile, reason, outcome)
            }
        }
    }

    fn unresolved_name(&self, name: &str, outcome: &mut super::Outcome) -> String {
        outcome.code = 1;
        outcome.diagnostics = format!("cx: unresolved name {name}\n");
        if self.display.compact() {
            return String::new();
        }
        self.rejection_page(false, "unresolved", None)
    }

    fn unavailable(
        &self,
        path: &str,
        profile: Option<Navigation>,
        reason: Unavailable,
        outcome: &mut super::Outcome,
    ) -> String {
        if self.display.public() {
            outcome.code = 2;
        }
        if self.display.compact() {
            outcome.diagnostics = reason.diagnostic().into();
            return String::new();
        }
        let indexed = reason.indexed();
        let context = indexed.then(|| context_key(self.pin, path, profile));
        self.rejection_page(indexed, reason.status(), context)
    }

    fn rejection_page(&self, indexed: bool, status: &str, context: Option<String>) -> String {
        let contexts = array(context.into_iter().map(|key| object(&[
            ("context", quote(&key)),
            ("coverage", quote("unsupported")),
            ("reason", quote("unsupported_operation")),
            ("bindings", "[]".into()),
        ])));
        self.response(indexed, status, contexts, JsonPage::default())
    }

    fn compact(
        &self,
        context: super::display::Context,
        selected: hits::Hits,
        unresolved: bool,
        outcome: &mut super::Outcome,
    ) -> io::Result<String> {
        if selected.truncated {
            outcome.diagnostics.push_str("cx: results truncated\n");
        }
        if unresolved {
            outcome.diagnostics.push_str("cx: unresolved position\n");
        }
        outcome
            .diagnostics
            .push_str("cx: partial reference coverage\n");
        let sites = selected
            .rows
            .into_keys()
            .map(|(f, b, _, _)| (f, b))
            .collect();
        super::display::TextOutput { pin: self.pin, cwd: self.cwd, context, ranked: false }.render(sites)
    }
    fn structured(
        &self,
        keys: ContextKeys<impl FnOnce() -> String>,
        selection: selection::Selection,
        selected: hits::Hits,
        symbols: &QuerySymbols<'_, impl ExtentRead>,
    ) -> io::Result<String> {
        let page = self.render_page(&keys, selected)?;
        let status = if selection.targets.is_empty() {
            "unresolved"
        } else {
            "resolved"
        };
        let contexts = if selection.contexts.is_empty() {
            array([symbols.context_json(&keys.fallback, selection.roles.unwrap_or_default())])
        } else {
            array(
                selection
                    .contexts
                    .into_iter()
                    .map(|(id, roles)| symbols.context_json(&keys.by_id[&id], roles)),
            )
        };
        Ok(self.response(true, status, contexts, page))
    }
}
#[derive(Default)]
struct JsonPage {
    hits: ObjectArray,
    evidence: ObjectArray,
    truncated: bool,
}

impl JsonPage {
    fn add_evidence(
        &mut self,
        role: &str,
        keys: &ContextKeys<impl FnOnce() -> String>,
        entries: impl IntoIterator<Item = (Option<usize>, Rc<str>)>,
    ) {
        entries.into_iter().for_each(|(ctx, target)| self.evidence(role, keys.for_hit(ctx), &target));
    }

    fn evidence(&mut self, role: &str, context: &str, target: &str) {
        self.evidence.push(|row| {
            row.number("hit", self.hits.len);
            row.string("context", context);
            row.raw("role", role);
            row.raw("certainty", r#""candidates""#);
            row.string("target", target);
        });
    }
}

struct ContextKeys<F: FnOnce() -> String> {
    by_id: BTreeMap<usize, String>,
    fallback: LazyCell<String, F>,
}

impl<F: FnOnce() -> String> ContextKeys<F> {
    fn for_hit(&self, id: Option<usize>) -> &str {
        id.and_then(|id| self.by_id.get(&id)).map_or_else(|| self.fallback.as_str(), String::as_str)
    }
}

impl Reply<'_> {
    fn render_page(
        &self,
        keys: &ContextKeys<impl FnOnce() -> String>,
        selected: hits::Hits,
    ) -> io::Result<JsonPage> {
        let Self { pin, op, .. } = *self;
        let role = match op {
            Operation::Callers => r#""call_target""#,
            Operation::Definition => r#""definition""#,
            Operation::References => r#""reference""#,
        };
        let mut page = JsonPage { truncated: selected.truncated, ..JsonPage::default() };
        let mut positions = super::display::Positions::default();
        for ((f, byte, symbol, caller), ids) in selected.rows {
            page.add_evidence(role, keys, ids);
            let position = positions.position(pin, f, byte)?;
            let caller_column = (op == Operation::Callers).then_some(caller.as_deref());
            page.hits.push(|row| {
                row.string("path", pin.files[f].path());
                row.number("line", position.line);
                row.number("col", position.utf16_column);
                row.nullable_string("symbol", symbol.as_deref());
                append_caller(row, caller_column);
            });
        }
        Ok(page)
    }
}

fn append_caller(row: &mut super::json::Object<&mut String>, caller: Option<Option<&str>>) {
    if let Some(value) = caller {
        row.nullable_string("caller", value);
    }
}

fn context_key(pin: &Pin, path: &str, profile: Option<Navigation>) -> String {
    let pack = profile.map_or("unknown", |p| p.pack);
    let manifest = profile.and_then(|p| p.manifest);
    let entry = manifest
        .and_then(|name| {
            Path::new(path).ancestors().skip(1).find_map(|dir| {
                let key = dir.join(name);
                let i = pin
                    .files
                    .binary_search_by(|f| f.path().cmp(key.to_str().unwrap()))
                    .ok()?;
                Some(pin.files[i].path())
            })
        })
        .unwrap_or(".");
    encode_context(pack, entry, "")
}

fn encode_context(pack: &str, entry: &str, variant: &str) -> String {
    use std::fmt::Write;
    let mut id = String::from("ctx1/l4/");
    for value in [pack, entry, variant] {
        write!(id, "s{}/", value.len()).unwrap();
        for byte in value.bytes() {
            write!(id, "{byte:02x}").unwrap();
        }
    }
    id.push_str("m0/");
    id
}
