//! Project scoring policy. Rules are ordered by declaration, and globs match
//! the complete repository-relative path. An omitted setting inherits.
pub(crate) mod directives;
use super::Rule;
use crate::domain::pattern::Glob;

pub(crate) const MARKER: &str = "cx-score:";

type Thresholds = [usize; Rule::ALL.len()];
const DEFAULTS: Thresholds = {
    let mut values = [0; Rule::ALL.len()];
    let mut i = 0;
    while i < values.len() {
        values[i] = Rule::ALL[i].default_threshold();
        i += 1;
    }
    values
};

// File-local scoring is synchronous. Guest-language scorers inherit this
// scope, while parallel index workers continue using their own defaults.
thread_local! {
    static THRESHOLDS: std::cell::Cell<Thresholds> = const { std::cell::Cell::new(DEFAULTS) };
}

pub(super) fn measurement_threshold(rule: Rule) -> usize {
    THRESHOLDS.get()[rule as usize]
}

struct RestoreThresholds(Thresholds);

impl Drop for RestoreThresholds {
    fn drop(&mut self) {
        THRESHOLDS.set(self.0);
    }
}

#[derive(Default)]
pub(crate) struct Project {
    sections: Vec<Section>,
}

struct Section {
    glob: Glob,
    settings: Vec<Setting>,
}

pub(crate) struct Setting {
    rule: Option<Rule>,
    pub(crate) threshold: Option<usize>,
    pub(crate) reason: String,
    pub(crate) line: usize,
}

impl Setting {
    fn parse(text: &str, line: usize) -> Result<Self, String> {
        let (assignment, reason) = text
            .split_once('#')
            .filter(|(_, reason)| !reason.trim().is_empty())
            .ok_or("expected # followed by the reason for this policy")?;
        let (key, value) = assignment
            .split_once('=')
            .ok_or("expected rule = threshold or off")?;
        let key = key.trim();
        let rule = match key {
            "file" => None,
            _ => Some(Rule::named(key).ok_or_else(|| format!("unknown scoring rule {key:?}"))?),
        };
        let threshold = match value.trim() {
            "off" => None,
            _ if rule.is_none() => return Err("file accepts only off".into()),
            n => Some(
                n.parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or("expected a positive threshold or off")?,
            ),
        };
        Ok(Self {
            rule,
            threshold,
            reason: reason.trim().into(),
            line,
        })
    }
}

#[derive(Default)]
pub(crate) struct FilePolicy<'a> {
    pub(crate) excluded: Option<&'a Setting>,
    pub(crate) rules: [Option<&'a Setting>; Rule::ALL.len()],
}

impl FilePolicy<'_> {
    pub(crate) fn threshold(&self, rule: Rule) -> usize {
        self.rules[rule as usize].map_or(rule.default_threshold(), |setting| {
            setting.threshold.unwrap_or(usize::MAX)
        })
    }

    pub(crate) fn enabled(&self, path: &str, test_rules: bool) -> u8 {
        let mut rules = if crate::domain::markdown::is_markdown(path) {
            1 << Rule::Length as u8
        } else {
            u8::MAX
        };
        if !test_rules || !super::Language::for_path(path).is_some_and(|lang|
            super::test_logic::supports(lang) && crate::domain::code::roles::on_test_path(path, lang)) {
            rules &= !(1 << Rule::Oracle as u8);
        }
        for rule in Rule::ALL {
            if self.rules[rule as usize].is_some_and(|setting| setting.threshold.is_none()) {
                rules &= !(1 << rule as u8);
            }
        }
        rules
    }

    pub(crate) fn needs_measurement(&self, cached: Option<&super::Score>) -> bool {
        Rule::ALL.into_iter().any(|rule| {
            self.rules[rule as usize]
                .and_then(|setting| setting.threshold)
                .is_some_and(|threshold| requires_rescan(rule, threshold, cached))
        })
    }

    pub(crate) fn measure<T>(&self, scan: impl FnOnce() -> T) -> T {
        let thresholds = Rule::ALL.map(|rule| self.threshold(rule));
        let _restore = RestoreThresholds(THRESHOLDS.replace(thresholds));
        scan()
    }
}

// Raised thresholds usually filter cached findings. For duplicates they
// select eligible partners, and for oracles they control when decisions reset.
fn requires_rescan(rule: Rule, threshold: usize, cached: Option<&super::Score>) -> bool {
    use std::cmp::Ordering;
    match threshold.cmp(&rule.default_threshold()) {
        Ordering::Less => true,
        Ordering::Greater if matches!(rule, Rule::Duplication | Rule::Oracle) => {
            cached.is_none_or(|score| score.findings.iter().any(|f| f.rule == rule && f.value < threshold))
        }
        _ => false,
    }
}

impl Project {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut project = Self::default();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            project
                .add(line, index + 1)
                .map_err(|error| format!(".cxconfig:{}: {error}", index + 1))?;
        }
        Ok(project)
    }

    fn add(&mut self, text: &str, line: usize) -> Result<(), String> {
        if let Some(pattern) = text.strip_prefix('[') {
            let glob = pattern
                .strip_suffix(']')
                .filter(|s| !s.is_empty())
                .ok_or("expected [repository-relative glob]")?;
            self.sections.push(Section {
                glob: Glob::new(glob)?,
                settings: Vec::new(),
            });
            return Ok(());
        }
        let section = self
            .sections
            .last_mut()
            .ok_or("expected [repository-relative glob] before setting")?;
        section.settings.push(Setting::parse(text, line)?);
        Ok(())
    }

    pub(crate) fn resolve(&self, path: &str) -> FilePolicy<'_> {
        self.resolve_with(path, &mut crate::domain::pattern::Scratch::default())
    }

    pub(crate) fn resolve_with(&self, path: &str, scratch: &mut crate::domain::pattern::Scratch) -> FilePolicy<'_> {
        let mut policy = FilePolicy::default();
        for section in self
            .sections
            .iter()
            .filter(|section| section.glob.matches_with(path, scratch))
        {
            for setting in &section.settings {
                let slot = match setting.rule {
                    None => &mut policy.excluded,
                    Some(rule) => &mut policy.rules[rule as usize],
                };
                *slot = Some(setting);
            }
        }
        policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_sections_override_only_the_named_rule() {
        let project = Project::parse(
            "[**/*.rs]\nlength=80 # code\ndepth=off # example\n[src/**]\nlength=40 # library",
        )
        .unwrap();
        let policy = project.resolve("src/lib.rs");
        let length = policy.rules[Rule::Length as usize].unwrap();
        assert_eq!(
            (length.threshold, length.reason.as_str(), length.line),
            (Some(40), "library", 5)
        );
        assert_eq!(
            policy.rules[Rule::Nesting as usize].unwrap().threshold,
            None
        );
        assert!(project.resolve("main.py").rules.iter().all(Option::is_none));
    }

    #[test]
    fn rejects_typos_missing_reasons_and_ambiguous_settings() {
        for text in [
            "wrong=off # typo",
            "[**]\nwrong=off # typo",
            "[**]\ndepth=off",
            "[**]\nfile=2 # invalid",
            "[**]\nlength=0 # invalid",
        ] {
            assert!(Project::parse(text).is_err(), "{text}");
        }
    }
}
