//! Project translation-local ordinals onto physical source rows for graph publication.
//! Keep each context's uses separate even when their written spans coincide.
use super::super::{Analysis, Call, Declaration, ForwardNames, Site, Unit, direct::Evidence};
use std::{collections::HashMap, io};
#[derive(Default)]
struct File {
    facts: Analysis,
    declarations: HashMap<(usize, usize, &'static str), usize>,
}

impl File {
    fn declaration(&mut self, d: &Declaration) -> usize {
        *self
            .declarations
            .entry((d.span.start, d.span.end, d.kind))
            .or_insert_with(|| {
                let local = self.facts.declarations.len();
                self.facts.declarations.push(Declaration {
                    kind: d.kind,
                    span: d.span.clone(),
                });
                local
            })
    }
}

#[derive(Default)]
pub(crate) struct Projection {
    keys: Vec<String>,
    files: HashMap<usize, File>,
    uses: Vec<(Site, usize, Option<Site>)>,
    edges: Vec<(Site, Site)>,
    definitions: Vec<(Site, usize)>,
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid translation-unit evidence",
    )
}

struct PartitionRows<'a, 'src> {
    units: &'a [Unit<'src>],
    files: Vec<usize>,
}

impl<'a, 'src> PartitionRows<'a, 'src> {
    fn new(units: &'a [Unit<'src>], paths: &[String]) -> io::Result<Self> {
        let files = paths
            .iter()
            .map(|path| units.binary_search_by_key(&path.as_str(), |unit| unit.path))
            .collect::<Result<_, _>>()
            .map_err(|_| invalid())?;
        Ok(Self { units, files })
    }

    fn file(&self, index: usize, span: &std::ops::Range<usize>) -> io::Result<usize> {
        let file = *self.files.get(index).ok_or_else(invalid)?;
        self.units[file]
            .text
            .get(span.clone())
            .ok_or_else(invalid)?;
        Ok(file)
    }
}

impl Projection {
    pub fn new(units: &[Unit]) -> io::Result<Self> {
        let mut projection = Self::default();
        let partitions = units.iter().filter_map(|unit| {
            let Evidence::Translation(parts) = unit.analysis.direct_evidence.as_deref()? else {
                return None;
            };
            Some(parts)
        });
        for partition in partitions.flatten() {
            projection.partition(units, partition)?;
        }
        Ok(projection)
    }

    fn partition(
        &mut self,
        units: &[Unit],
        partition: &super::translation::Partition,
    ) -> io::Result<()> {
        let context = self.keys.len();
        self.keys.push(partition.key.clone());
        let rows = PartitionRows::new(units, &partition.paths)?;
        for &file in &rows.files {
            self.files.entry(file).or_default();
        }
        let mut declarations = Vec::with_capacity(partition.declarations.len());
        for (index, declaration) in &partition.declarations {
            let file = rows.file(*index, &declaration.span)?;
            let local = self.files.get_mut(&file).unwrap().declaration(declaration);
            let site = Site { file, local };
            declarations.push(site);
            self.definitions.push((site, context));
        }
        let mut calls = Vec::with_capacity(partition.calls.len());
        for (index, call) in &partition.calls {
            let file = rows.file(*index, &call.span)?;
            let site = self.call(file, call);
            let owner = call
                .owner
                .map(|id| declarations.get(id).copied().ok_or_else(invalid))
                .transpose()?;
            calls.push(site);
            self.uses.push((site, context, owner));
        }
        self.publish_edges(&partition.edges, &calls, &declarations)
    }

    fn call(&mut self, file: usize, call: &Call) -> Site {
        let facts = &mut self.files.get_mut(&file).unwrap().facts;
        let local = facts.calls.len();
        facts.calls.push(Call {
            span: call.span.clone(),
            is_call: call.is_call,
            owner: None,
        });
        Site { file, local }
    }

    fn publish_edges(
        &mut self,
        edges: &[(usize, Vec<usize>)],
        calls: &[Site],
        declarations: &[Site],
    ) -> io::Result<()> {
        for (local, targets) in edges.iter().filter(|(_, targets)| !targets.is_empty()) {
            let source = *calls.get(*local).ok_or_else(invalid)?;
            for target in targets {
                self.edges
                    .push((source, *declarations.get(*target).ok_or_else(invalid)?));
            }
        }
        Ok(())
    }

    pub fn units<'a>(&'a self, original: &'a [Unit<'a>]) -> Option<Vec<Unit<'a>>> {
        (!self.files.is_empty()).then(|| {
            original
                .iter()
                .enumerate()
                .map(|(i, u)| Unit {
                    path: u.path,
                    text: u.text,
                    analysis: self.files.get(&i).map_or(u.analysis, |f| &f.facts),
                })
                .collect()
        })
    }
    pub fn bind(&self, names: &mut ForwardNames) {
        names.context_keys.clone_from(&self.keys);
        for &(source, target) in &self.edges {
            names.bind_one(source, target);
        }
        for &(site, context, owner) in &self.uses {
            names.translation_uses.insert(site, (context, owner));
        }
        for &(site, context) in &self.definitions {
            let contexts = names.translation_definitions.entry(site).or_default();
            if contexts.last() != Some(&context) {
                contexts.push(context);
            }
        }
    }
}
