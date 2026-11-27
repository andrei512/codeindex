//! Resolve one script file in source order, sharing the snapshot’s export cache.
use super::*;

pub(super) struct File<'a, 'cache> {
    id: usize,
    unit: &'a Unit<'a>,
    evidence: &'a Evidence,
    units: &'a [Unit<'a>],
    links: &'a ModuleLinks,
    exports: &'cache mut exports::Cache<'a>,
    last_callee: Option<(usize, &'a str, Option<Site>)>,
    longest_import: std::cell::OnceCell<usize>,
    wildcard_scopes: std::cell::OnceCell<WildcardScopes>,
}

struct UseRow<'a, 'names> {
    unit: &'a Unit<'a>,
    file: usize,
    prior: &'names ForwardNames,
    local: usize,
}

impl<'a> UseRow<'a, '_> {
    fn spelling(&self) -> &'a str {
        self.text(&self.unit.analysis.calls[self.local].span)
    }

    fn text(&self, span: &std::ops::Range<usize>) -> &'a str {
        &self.unit.text[span.clone()]
    }

    fn prior_row(&self, local: usize) -> &[Site] {
        self.prior.row(Site {
            file: self.file,
            local,
        })
    }

    fn unique_prior(&self, local: usize) -> Option<Site> {
        let [target] = self.prior_row(local) else {
            return None;
        };
        Some(*target)
    }

    fn first_prior(&self, local: usize) -> Option<(Site, bool)> {
        Some((
            *self.prior_row(local).first()?,
            self.unit.analysis.calls[local].is_call,
        ))
    }
}

impl<'a, 'cache> File<'a, 'cache> {
    pub(super) fn new(
        id: usize,
        units: &'a [Unit<'a>],
        links: &'a ModuleLinks,
        exports: &'cache mut exports::Cache<'a>,
    ) -> Option<Self> {
        let unit = &units[id];
        Some(Self {
            id,
            unit,
            evidence: unit.analysis.script.as_deref()?,
            units,
            links,
            exports,
            last_callee: None,
            longest_import: Default::default(),
            wildcard_scopes: Default::default(),
        })
    }

    pub(super) fn bind(mut self, out: &mut ForwardNames) {
        for (local, lookup) in self.evidence.uses.iter().enumerate() {
            let Some(target) = self.target(local, lookup, out) else {
                continue;
            };
            let source = Site { file: self.id, local };
            self.bind_family(out, source, target);
        }
    }

    fn bind_family(&self, out: &mut ForwardNames, source: Site, target: Site) {
        let family = self.units[target.file]
            .analysis
            .script
            .as_ref()
            .and_then(|facts| facts.families.get(&target.local))
            .filter(|family| !family.is_empty());
        let Some(family) = family else {
            out.bind_one(source, target);
            return;
        };
        let mut targets = Vec::with_capacity(1 + family.len());
        targets.push(target);
        targets.extend(family.iter().map(|&local| Site {
            file: target.file,
            local,
        }));
        out.bind(source, targets);
    }

    fn binding(&mut self, scope: usize, name: &str) -> Option<Site> {
        binding_target(self.id, self.evidence.binding(scope, name), |n| {
            self.exports.get(self.id, n, None)
        })
    }

    /// Keyword arguments and JSX properties share a lexical callee binding.
    /// Within this snapshot its identity depends only on the scope and spelling.
    fn callee(&mut self, scope: usize, name: &'a str) -> Option<Site> {
        match self.last_callee {
            Some((prior, spelling, target)) if prior == scope && spelling == name => target,
            _ => {
                let target = self.binding(scope, name);
                self.last_callee = Some((scope, name, target));
                target
            }
        }
    }

    fn callee_parameter(&mut self, scope: usize, callee: &'a str, name: &str) -> Option<Site> {
        let function = self.callee(scope, callee)?;
        let evidence = self.units[function.file].analysis.script.as_deref()?;
        let scope = *evidence.functions.get(&function.local)?;
        let Binding::Local(local) = *evidence.scopes[scope].bindings.get(name)? else {
            return None;
        };
        let file = function.file;
        (self.units[file].analysis.declarations[local].kind == "parameter")
            .then_some(Site { file, local })
    }

    fn property(&self, function: Site, name: &str) -> Option<Site> {
        let evidence = self.units[function.file].analysis.script.as_deref()?;
        let shape = evidence.props.get(&function.local)?;
        Some(Site {
            file: function.file,
            local: evidence.members.get(*shape, name)?,
        })
    }
    fn member(&mut self, owner: Site, name: &str, called: bool) -> Option<Site> {
        member(self.units, MemberLookup { owner, name, called }, |f, n| {
            self.exports.get(f, n, None)
        })
    }

    fn target(
        &mut self,
        local: usize,
        lookup: &Lookup,
        out: &ForwardNames,
    ) -> Option<Site> {
        let file = self.id;
        let row = UseRow {
            unit: self.unit,
            file,
            prior: out,
            local,
        };
        let name = row.spelling();
        match lookup {
            Lookup::Super { scope, base_scope, base } => {
                self.super_member(*scope, *base_scope, row.text(base), name)
            }
            Lookup::Construct { scope } => self.constructor(*scope, name),
            Lookup::Chained { prior, scope, qualification } => {
                self.chained(&row, *prior, *scope, qualification.as_ref())
            }
            Lookup::Known(_) | Lookup::Imported(_) => self.explicit_target(lookup),
            Lookup::Prop { .. } | Lookup::PropOf(_) => self.target_property(&row, lookup),
            Lookup::Argument { scope, callee } => {
                self.callee_parameter(*scope, row.text(callee), name)
            }
            Lookup::Name { scope, receiver } => {
                self.name(*scope, name, receiver.as_ref().map(|r| row.text(r)))
            }
        }
    }

    fn explicit_target(&mut self, lookup: &Lookup) -> Option<Site> {
        let file = self.id;
        match lookup {
            Lookup::Known(local) => Some(Site {
                file,
                local: *local,
            }),
            Lookup::Imported(n) => self.exports.get(file, *n, None),
            _ => None,
        }
    }

    fn target_property(&mut self, row: &UseRow<'a, '_>, lookup: &Lookup) -> Option<Site> {
        let function = match lookup {
            Lookup::Prop { scope, component } => self.callee(*scope, row.text(component)),
            Lookup::PropOf(prior) => row.unique_prior(*prior),
            _ => None,
        }?;
        self.property(function, row.spelling())
    }

    fn super_member(
        &mut self,
        scope: usize,
        base_scope: usize,
        base: &str,
        name: &str,
    ) -> Option<Site> {
        // Only the builtin zero-argument form on a single written base.
        if self.evidence.binding(scope, "super").is_some() {
            return None;
        }
        let owner = self.binding(base_scope, base)?;
        if self.units[owner.file].analysis.declarations[owner.local].kind != "class_definition" {
            return None;
        }
        self.member(owner, name, false)
    }

    fn constructor(&mut self, scope: usize, name: &str) -> Option<Site> {
        let owner = self.binding(scope, name)?;
        let evidence = self.units[owner.file].analysis.script.as_ref().unwrap();
        let constructor = evidence
            .shapes
            .get(&owner.local)
            .and_then(|shape| evidence.members.get(*shape, "constructor"));
        Some(Site {
            file: owner.file,
            local: constructor.unwrap_or(owner.local),
        })
    }

    fn chained(
        &mut self,
        row: &UseRow<'a, '_>,
        prior: usize,
        scope: usize,
        qualification: Option<&std::ops::Range<usize>>,
    ) -> Option<Site> {
        let name = row.spelling();
        let package = qualification.and_then(|span| {
            package_import(self.evidence, scope, row.text(span), &self.longest_import)
        });
        if let Some(import) = package {
            return self.exports.get(self.id, import, Some(name));
        }
        let (owner, called) = row.first_prior(prior)?;
        self.member(owner, name, called)
    }

    fn name(&mut self, scope: usize, name: &'a str, receiver: Option<&str>) -> Option<Site> {
        match receiver {
            Some(receiver) => self.receiver(scope, receiver, name),
            None => self.lexical(scope, name),
        }
    }

    fn lexical(&mut self, scope: usize, name: &'a str) -> Option<Site> {
        let evidence = self.evidence;
        let file = self.id;
        if let Some(binding) = evidence.binding(scope, name) {
            return binding_target(file, Some(binding), |import| self.exports.get(file, import, None));
        }
        if evidence.wildcards.is_empty() {
            return None;
        }
        let scopes = self.wildcard_scopes.get_or_init(|| WildcardScopes::index(&evidence.wildcards));
        scopes.resolve(evidence, scope, |import| {
            let remote = self.units[self.links.imports[import]?].analysis.script.as_ref()?;
            if !remote.public_wildcard(name) {
                return None;
            }
            self.exports.get(file, import, Some(name))
        })
    }

    fn receiver(&mut self, scope: usize, receiver: &str, name: &'a str) -> Option<Site> {
        let evidence = self.evidence;
        if receiver == "this" && let Some(shape) = this_shape(evidence, scope) {
            return self.member_of_shape(shape, name);
        }
        match *evidence.binding(scope, receiver)? {
            Binding::Local(local) => self.local_receiver(receiver, local, name),
            Binding::Import(import) | Binding::Namespace { import, .. } => self.import_receiver(import, name),
            Binding::Package | Binding::Conflicting => None,
        }
    }

    fn member_of_shape(&self, shape: usize, name: &str) -> Option<Site> {
        self.evidence.members.get(shape, name).map(|local| Site { file: self.id, local })
    }

    fn local_receiver(&mut self, receiver: &str, local: usize, name: &str) -> Option<Site> {
        if Evidence::receiver_parameter(receiver) && let Some(&shape) = self.evidence.self_params.get(&local) {
            return self.member_of_shape(shape, name);
        }
        self.member(Site { file: self.id, local }, name, false)
    }

    fn import_receiver(&mut self, import: usize, name: &'a str) -> Option<Site> {
        if let Some(target) = self.exports.get(self.id, import, Some(name)) {
            return Some(target);
        }
        let owner = self.exports.get(self.id, import, None)?;
        if self.units[owner.file].analysis.declarations[owner.local].kind != "module_declaration" {
            return None;
        }
        self.member(owner, name, false)
    }
}

fn this_shape(e: &Evidence, mut scope: usize) -> Option<usize> {
    loop {
        let current = &e.scopes[scope];
        if current.receiver.is_some() || current.this_boundary {
            return current.receiver;
        }
        scope = current.parent?;
    }
}
