//! Resolve independent paths in parallel. Receiver actions that read earlier
//! inference results are applied in file and use order by the coordinator.
use super::*;

pub(super) enum Action {
    One(Site),
    Resolved(Vec<Site>),
    Receiver(usize),
}
impl Action {
    fn targets(targets: Vec<Site>) -> Option<Self> {
        match targets.as_slice() {
            [] => None,
            [target] => Some(Self::One(*target)),
            _ => Some(Self::Resolved(targets)),
        }
    }

    fn sites(&self) -> Option<&[Site]> {
        match self {
            Self::One(target) => Some(std::slice::from_ref(target)),
            Self::Resolved(targets) => Some(targets),
            Self::Receiver(_) => None,
        }
    }
}

/// Actions are produced in increasing use-row order. The usual receiver is the
/// immediately preceding action; a nested expression can reference an older one.
fn prepared_sites(pending: &[(usize, Action)], local: usize) -> Option<&[Site]> {
    let action = match pending.last() {
        Some((row, action)) if *row == local => Some(action),
        _ => pending
            .binary_search_by_key(&local, |(row, _)| *row)
            .ok()
            .map(|i| &pending[i].1),
    };
    action.map_or(Some(&[]), Action::sites)
}

fn terminal_types(
    r: &Resolver,
    members: &members::Table,
    cache: &mut HashMap<Site, Option<Vec<Site>>>,
    sites: impl Iterator<Item = Site>,
) -> Option<Vec<Site>> {
    let mut types = Vec::new();
    for site in sites {
        let terminal = cache
            .entry(site)
            .or_insert_with(|| members.terminal(r, site));
        types.extend_from_slice(terminal.as_ref()?);
    }
    Some(types)
}

type Prepared = Vec<(usize, Vec<(usize, Action)>)>;

pub(super) fn prepare_paths(
    files: &[usize],
    r: &Resolver,
    members: &members::Table,
    names: &ForwardNames,
) -> Vec<Prepared> {
    let total: usize = files.iter().map(|&file| r.evidence(file).uses.len()).sum();
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(total.div_ceil(4096))
        .max(1);
    if workers == 1 {
        return vec![prepare_range(r, members, names, files)];
    }
    let budget = total.div_ceil(workers);
    std::thread::scope(|scope| {
        let mut jobs = Vec::new();
        let mut begin = 0;
        let mut cost = 0;
        for (at, &file) in files.iter().enumerate() {
            cost += r.evidence(file).uses.len();
            if cost >= budget || at + 1 == files.len() {
                let batch = &files[begin..=at];
                let worker = Resolver {
                    state: Arc::clone(&r.state),
                    module_results: Default::default(),
                };
                jobs.push(scope.spawn(move || prepare_range(&worker, members, names, batch)));
                begin = at + 1;
                cost = 0;
            }
        }
        jobs.into_iter()
            .map(|job| job.join().expect("path preparation worker"))
            .collect()
    })
}

fn prepare_range(
    r: &Resolver,
    members: &members::Table,
    names: &ForwardNames,
    files: &[usize],
) -> Prepared {
    let mut worker = Preparation {
        members,
        names,
        terminal: HashMap::new(),
    };
    files
        .iter()
        .filter_map(|&file| {
            let actions = worker.file(r, file);
            (!actions.is_empty()).then_some((file, actions))
        })
        .collect()
}

// Terminal results are independent of the use row and can be reused across
// every file handled by this worker. Pending actions belong to one file.
struct Preparation<'a, 'src> {
    members: &'a members::Table<'src>,
    names: &'a ForwardNames,
    terminal: HashMap<Site, Option<Vec<Site>>>,
}
impl Preparation<'_, '_> {
    fn file(&mut self, r: &Resolver, file: usize) -> Vec<(usize, Action)> {
        let u = &r.units[file];
        let e = r.evidence(file);
        let mut pending = Vec::new();
        let active =
            (0..e.uses.len()).filter(|&local| r.active(file, u.analysis.calls[local].span.start));
        for local in active {
            let action = self.action(r, Site { file, local }, &pending);
            pending.extend(action.map(|action| (local, action)));
        }
        pending
    }

    fn action(
        &mut self,
        r: &Resolver,
        source: Site,
        pending: &[(usize, Action)],
    ) -> Option<Action> {
        let Site { file, local } = source;
        let evidence = r.evidence(file);
        if let Some(&local) = evidence.direct.get(&local) {
            return Some(Action::One(Site { file, local }));
        }
        if let Some(receiver) = evidence.members.uses.get(&local) {
            return self.member(r, source, receiver, pending);
        }
        let unit = &r.units[file];
        let call = &unit.analysis.calls[local];
        let fact = &evidence.uses[local];
        let (head, tail) = fact.spelling(&unit.text[call.span.clone()]);
        if head == "Self" && tail.is_empty() {
            return None;
        }
        let mut targets = self.members.path(
            r,
            members::PathLookup {
                at: (file, fact.scope),
                offset: call.span.start,
                namespace: fact.ns,
                spelling: (head, tail),
            },
        );
        targets.retain(|t| {
            let d = &r.units[t.file].analysis.declarations[t.local];
            r.active_declaration(*t) && d.kind != "field_declaration"
        });
        Action::targets(targets)
    }

    // Literal types are independent. An expression receiver can be bound now
    // only when every preceding target has a terminal type.
    fn member(
        &mut self,
        r: &Resolver,
        source: Site,
        receiver: &members::Receiver,
        pending: &[(usize, Action)],
    ) -> Option<Action> {
        let file = source.file;
        let types = match receiver {
            members::Receiver::Literal(path) => {
                let scope = r.evidence(file).uses[source.local].scope;
                let offset = r.units[file].analysis.calls[source.local].span.start;
                self.members.path(
                    r,
                    members::PathLookup {
                        at: (file, scope),
                        offset,
                        namespace: Namespace::Type,
                        spelling: (&path[0], &path[1..]),
                    },
                )
            }
            members::Receiver::Previous(previous) => {
                let original = self.names.row(Site {
                    file,
                    local: *previous,
                });
                let types = prepared_sites(pending, *previous).and_then(|rows| {
                    terminal_types(
                        r,
                        self.members,
                        &mut self.terminal,
                        rows.iter().chain(original).copied(),
                    )
                });
                let Some(types) = types else {
                    return Some(Action::Receiver(*previous));
                };
                types
            }
        };
        Action::targets(self.members.select(r, source, &types, receiver))
    }
}
