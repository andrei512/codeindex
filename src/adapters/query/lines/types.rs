//! File-type rules use ripgrep 14.1.1's published --type-list (Unlicense/MIT).
use super::super::Glob;
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Default)]
pub(super) struct Types {
    groups: Vec<(bool, Vec<Glob>)>,
    any_positive: bool,
    pub listing: Option<String>,
}

const BUILTINS: &str = include_str!("rg14-types.txt");
const BAD: &str = "invalid definition (format is type:glob, e.g., html:*.html)";

pub(super) enum Edit<'a> {
    Add(&'a str),
    Clear(&'a str),
}

impl Types {
    pub fn build(
        edits: Vec<Edit<'_>>,
        selections: Vec<(bool, &str)>,
        list: bool,
    ) -> Result<Self, String> {
        let mut types = Self::default();
        if edits.is_empty() && selections.is_empty() {
            if list {
                types.listing = Some(BUILTINS.to_owned());
            }
            return Ok(types);
        }
        let mut catalog: BTreeMap<_, Vec<_>> = BUILTINS
            .lines()
            .map(|line| {
                let (name, globs) = line.split_once(": ").unwrap();
                (name, globs.split(", ").collect())
            })
            .collect();
        for edit in edits {
            match edit {
                Edit::Clear(name) => {
                    catalog.remove(name);
                }
                Edit::Add(spec) => {
                    let (name, definition) = spec.split_once(':').ok_or(BAD)?;
                    if name.is_empty()
                        || name == "all"
                        || !name.chars().all(char::is_alphanumeric)
                        || definition.is_empty()
                    {
                        return Err(BAD.into());
                    }
                    if let Some(names) = definition.strip_prefix("include:") {
                        let mut inherited = Vec::new();
                        for source in names.split(',') {
                            inherited.extend_from_slice(catalog.get(source).ok_or(BAD)?);
                        }
                        catalog.entry(name).or_default().extend(inherited);
                    } else {
                        if definition.contains(':') {
                            return Err(BAD.into());
                        }
                        catalog.entry(name).or_default().push(definition);
                    }
                }
            }
        }
        for (positive, name) in selections {
            let mut compiled = Vec::new();
            if name == "all" {
                for pattern in catalog.values().flatten() {
                    compiled.push(Glob::new(pattern)?);
                }
            } else {
                for pattern in catalog
                    .get(name)
                    .ok_or_else(|| format!("unrecognized file type: {name}"))?
                {
                    compiled.push(Glob::new(pattern)?);
                }
            }
            types.any_positive |= positive;
            types.groups.push((positive, compiled));
        }
        if list {
            let mut output = String::new();
            for (name, mut patterns) in catalog {
                patterns.sort_unstable();
                writeln!(output, "{name}: {}", patterns.join(", ")).unwrap();
            }
            types.listing = Some(output);
        }
        Ok(types)
    }

    pub fn accepts(&self, name: &str) -> bool {
        for (positive, patterns) in self.groups.iter().rev() {
            if patterns.iter().any(|glob| glob.is_match(name)) {
                return *positive;
            }
        }
        !self.any_positive
    }
}
