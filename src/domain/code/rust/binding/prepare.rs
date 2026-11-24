//! Resolve independent paths in parallel. Receiver actions that read earlier
//! inference results are applied in file and use order by the coordinator.
use super::*;

pub(super) enum Action {
    One(Site),
    Resolved(Vec<Site>),
    Receiver,
}
type Prepared = Vec<(usize, Vec<(usize, Action)>)>;

pub(super) fn prepare_paths(
    r: &Resolver,
    members: &members::Table,
    names: &ForwardNames,
) -> Vec<Prepared> {
    let total: usize = r
        .units
        .iter()
        .filter_map(|u| u.analysis.rust.as_ref())
        .map(|e| e.uses.len())
        .sum();
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(total.div_ceil(4096))
        .max(1);
    if workers == 1 {
        return vec![prepare_range(r, members, names, 0..r.units.len())];
    }
    let budget = total.div_ceil(workers);
    std::thread::scope(|scope| {
        let mut jobs = Vec::new();
        let mut begin = 0;
        let mut cost = 0;
        for (file, u) in r.units.iter().enumerate() {
            cost += u.analysis.rust.as_ref().map_or(0, |e| e.uses.len());
            if cost >= budget || file + 1 == r.units.len() {
                let files = begin..file + 1;
                let worker = Resolver {
                    state: Arc::clone(&r.state),
                    module_results: Default::default(),
                };
                jobs.push(scope.spawn(move || prepare_range(&worker, members, names, files)));
                begin = file + 1;
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
    range: std::ops::Range<usize>,
) -> Prepared {
    let mut out = Vec::new();
    let mut terminal = HashMap::new();
    for file in range {
        let u = &r.units[file];
        let Some(e) = &u.analysis.rust else { continue };
        let mut pending = Vec::<(usize, Action)>::new();
        let mut positions = vec![usize::MAX; e.uses.len()];
        for (local, v) in e.uses.iter().enumerate() {
            let c = &u.analysis.calls[local];
            if !r.active(file, c.span.start) {
                continue;
            }
            let before = pending.len();
            if let Some(&target) = e.direct.get(&local) {
                pending.push((
                    local,
                    Action::One(Site {
                        file,
                        local: target,
                    }),
                ));
            } else if let Some(receiver) = e.members.uses.get(&local) {
                let at = (file, v.scope);
                let key = v
                    .path
                    .first()
                    .map(String::as_str)
                    .unwrap_or_else(|| name(&u.text[c.span.clone()]));
                let types = match receiver {
                    members::Receiver::Literal(path) => Some(members.path(
                        r,
                        at,
                        &path[0],
                        &path[1..],
                        c.span.start,
                        Namespace::Type,
                    )),
                    members::Receiver::Previous(prev) => {
                        let mut types = Vec::new();
                        let original = names.row(Site { file, local: *prev });
                        let sources = positions
                            .get(*prev)
                            .and_then(|&i| pending.get(i))
                            .map(|(_, action)| action);
                        let rows = match sources {
                            None => Some(&[][..]),
                            Some(Action::One(target)) => Some(std::slice::from_ref(target)),
                            Some(Action::Resolved(targets)) => Some(targets.as_slice()),
                            Some(Action::Receiver) => None,
                        };
                        rows.and_then(|rows| {
                            for &site in rows.iter().chain(original) {
                                let item = terminal
                                    .entry(site)
                                    .or_insert_with(|| members.terminal(r, site));
                                types.extend_from_slice(item.as_ref()?);
                            }
                            Some(types)
                        })
                    }
                };
                if let Some(types) = types {
                    let mut targets = members.select(r, at, key, c.span.start, &types);
                    targets.retain(|t| {
                        let d = &r.units[t.file].analysis.declarations[t.local];
                        r.active(t.file, d.span.start)
                            && match receiver {
                                members::Receiver::Previous(_) if c.is_call => {
                                    d.kind.starts_with("function")
                                }
                                _ => d.kind == "field_declaration",
                            }
                    });
                    if let [target] = targets.as_slice() {
                        pending.push((local, Action::One(*target)));
                    } else if !targets.is_empty() {
                        pending.push((local, Action::Resolved(targets)));
                    }
                } else {
                    pending.push((local, Action::Receiver));
                }
            } else {
                let (head, tail) = v
                    .path
                    .split_first()
                    .map(|(h, t)| (h.as_str(), t))
                    .unwrap_or_else(|| (name(&u.text[c.span.clone()]), &[]));
                if head == "Self" && tail.is_empty() {
                    continue;
                }
                let mut targets = members.path(r, (file, v.scope), head, tail, c.span.start, v.ns);
                targets.retain(|t| {
                    let d = &r.units[t.file].analysis.declarations[t.local];
                    r.active(t.file, d.span.start) && d.kind != "field_declaration"
                });
                if let [target] = targets.as_slice() {
                    pending.push((local, Action::One(*target)));
                } else if !targets.is_empty() {
                    pending.push((local, Action::Resolved(targets)));
                }
            }
            if pending.len() > before {
                positions[local] = before;
            }
        }
        if !pending.is_empty() {
            out.push((file, pending));
        }
    }
    out
}
