//! File-type rules use ripgrep 14.1.1's published --type-list (Unlicense/MIT).
use super::super::Glob;
mod matchers;
use matchers::{Choice, Matchers};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

#[derive(Default)]
pub(super) struct Types {
    matchers: Matchers,
    any_positive: bool,
    pub listing: Option<String>,
}

const BUILTINS: &str = include_str!("rg14-types.txt");
const BAD: &str = "invalid definition (format is type:glob, e.g., html:*.html)";

pub(super) enum Edit<'a> {
    Add(&'a str),
    Clear(&'a str),
}

type Catalog<'a> = BTreeMap<&'a str, Vec<&'a str>>;

enum Definition<'a> {
    Include(&'a str),
    Glob(&'a str),
}

fn type_definition(spec: &str) -> Result<(&str, Definition<'_>), String> {
    let (name, definition) = spec.split_once(':').ok_or(BAD)?;
    let valid_name = !name.is_empty() && name.chars().all(char::is_alphanumeric);
    if !valid_name || name == "all" {
        return Err(BAD.into());
    }
    Ok((name, Definition::parse(definition)?))
}

impl<'a> Definition<'a> {
    fn parse(definition: &'a str) -> Result<Self, String> {
        if let Some(names) = definition.strip_prefix("include:") {
            return Ok(Self::Include(names));
        }
        if definition.is_empty() || definition.contains(':') {
            return Err(BAD.into());
        }
        Ok(Self::Glob(definition))
    }
}

fn apply<'a>(catalog: &mut Catalog<'a>, edit: Edit<'a>) -> Result<(), String> {
    let spec = match edit {
        Edit::Add(spec) => spec,
        Edit::Clear(name) => {
            catalog.remove(name);
            return Ok(());
        }
    };
    let (name, definition) = type_definition(spec)?;
    match definition {
        Definition::Include(names) => {
            let mut inherited = Vec::new();
            for source in names.split(',') {
                inherited.extend_from_slice(catalog.get(source).ok_or(BAD)?);
            }
            catalog.entry(name).or_default().extend(inherited);
        }
        Definition::Glob(glob) => catalog.entry(name).or_default().push(glob),
    }
    Ok(())
}

#[derive(Default)]
struct Selection<'a> {
    indexes: HashMap<&'a str, usize>,
    patterns: Vec<(Choice, Glob)>,
    any_positive: bool,
}

impl<'a> Selection<'a> {
    fn apply(&mut self, catalog: &Catalog<'a>, name: &str, choice: Choice) -> Result<(), String> {
        let mut select = |&pattern| self.select(pattern, choice);
        if name == "all" {
            catalog.values().flatten().try_for_each(select)?;
        } else {
            catalog.get(name).ok_or_else(|| format!("unrecognized file type: {name}"))?
                .iter().try_for_each(&mut select)?;
        }
        self.any_positive |= choice.positive;
        Ok(())
    }

    fn select(&mut self, pattern: &'a str, choice: Choice) -> Result<(), String> {
        use std::collections::hash_map::Entry;
        match self.indexes.entry(pattern) {
            Entry::Occupied(entry) => self.patterns[*entry.get()].0 = choice,
            Entry::Vacant(entry) => {
                entry.insert(self.patterns.len());
                self.patterns.push((choice, Glob::new(pattern)?));
            }
        }
        Ok(())
    }
}

fn listing(catalog: Catalog<'_>) -> String {
    let mut output = String::new();
    for (name, mut patterns) in catalog {
        patterns.sort_unstable();
        writeln!(output, "{name}: {}", patterns.join(", ")).unwrap();
    }
    output
}

fn builtin_catalog<'a>(include_all: bool, selections: &[(bool, &'a str)]) -> Catalog<'a> {
    let include_all = include_all || selections.iter().any(|&(_, name)| name == "all");
    BUILTINS.lines().filter_map(|line| {
        let (name, globs) = line.split_once(": ").unwrap();
        (include_all || selections.iter().any(|&(_, wanted)| wanted == name))
            .then(|| (name, globs.split(", ").collect()))
    }).collect()
}

impl Types {
    pub fn build(
        edits: Vec<Edit<'_>>,
        selections: Vec<(bool, &str)>,
        list: bool,
    ) -> Result<Self, String> {
        if edits.is_empty() && selections.is_empty() {
            return Ok(Self { listing: list.then(|| BUILTINS.to_owned()), ..Self::default() });
        }
        let mut catalog = builtin_catalog(!edits.is_empty() || list, &selections);
        for edit in edits {
            apply(&mut catalog, edit)?;
        }
        let mut selected = Selection::default();
        for (order, (positive, name)) in selections.into_iter().enumerate() {
            selected.apply(&catalog, name, Choice { order, positive })?;
        }
        Ok(Self {
            any_positive: selected.any_positive,
            matchers: Matchers::new(selected.patterns),
            listing: list.then(|| listing(catalog)),
        })
    }

    pub fn accepts(&self, name: &str) -> bool {
        self.matchers.accepts(name, !self.any_positive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_types_override_overlapping_literal_extension_and_wildcard_rules() {
        let selected = Types::build(
            vec![
                Edit::Add("wide:*.ts"),
                Edit::Add("wide:README*"),
                Edit::Add("narrow:README.ts"),
                Edit::Add("narrow:*.spec.ts"),
            ],
            vec![(false, "wide"), (true, "narrow"), (false, "ts"), (true, "narrow")],
            false,
        ).unwrap();
        assert!(selected.accepts("README.ts"));
        assert!(selected.accepts("unit.spec.ts"));
        assert!(!selected.accepts("main.ts"));
        assert!(!selected.accepts("README.txt"));
        assert!(!selected.accepts("unrelated"));
    }
}
