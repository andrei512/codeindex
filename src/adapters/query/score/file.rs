//! A single file borrows its catalog score unless project policy changes it.
use super::*;
use crate::domain::code::score::Finding;
use std::borrow::Cow;

pub(super) struct FileScore<'a> {
    pub path: &'a str,
    score: Option<Cow<'a, Score>>,
    selected: Option<Vec<usize>>,
    pub excluded_lines: usize,
    pub excluded_file: bool,
    pub value: Option<f64>,
    policy: FilePolicy<'a>,
    directives: Directives,
    empty: bool,
}

enum Measurement {
    Saved,
    Markdown,
    Source,
}

impl<'a> FileScore<'a> {
    pub fn read(
        pin: &'a Pin,
        id: usize,
        policy: FilePolicy<'a>,
        options: ScoreOptions,
    ) -> io::Result<Self> {
        let entry = &pin.files[id];
        let baseline = (if options.test_rules {
            entry.test_score.as_deref()
        } else {
            None
        })
        .or(entry.score.as_deref());
        let mut file = Self {
            path: entry.path(),
            empty: baseline.is_some(),
            value: None,
            score: baseline.filter(|s| s.lines > 0).map(Cow::Borrowed),
            excluded_file: policy.excluded.is_some(),
            excluded_lines: 0,
            selected: None,
            policy,
            directives: Directives::default(),
        };
        if file.excluded_file {
            file.excluded_lines = file.score.take().map_or(0, |s| s.lines);
            return Ok(file);
        }
        file.configure(pin, id, baseline, options)?;
        file.value = file.score.as_deref().map(|score| match &file.selected {
            None => score.value(),
            Some(ids) => Score::value_of(ids.iter().map(|&i| &score.findings[i])),
        });
        Ok(file)
    }

    fn configure(&mut self, pin: &Pin, id: usize, baseline: Option<&Score>, options: ScoreOptions) -> io::Result<()> {
        let marked = options.project_policy && pin.files[id].score_marker;
        let settings = self.policy.rules.iter().any(Option::is_some);
        if !marked && !settings {
            return Ok(());
        }
        let enabled = self.policy.enabled(self.path, options.test_rules);
        let remeasure = self.remeasure(pin, baseline, settings);
        if marked || remeasure {
            let text = pin.source_text(id)?;
            if marked {
                self.directives = Directives::read(self.path, &text, enabled).map_err(io::Error::other)?;
            }
            self.measure(pin, id, &text, (remeasure, options.test_rules))?;
        }
        self.apply(enabled);
        Ok(())
    }

    fn remeasure(&self, pin: &Pin, baseline: Option<&Score>, settings: bool) -> bool {
        let eligible = settings && !pin.is_memory();
        eligible && self.score.is_some() && self.policy.needs_measurement(baseline)
    }

    fn measurement(&self, remeasure: bool) -> Measurement {
        // Markdown policy comments are metadata and cannot add measured lines.
        let markers = !self.directives.markers.is_empty() && crate::domain::markdown::is_markdown(self.path);
        let has_findings = || self.score.as_ref().is_some_and(|score| !score.findings.is_empty());
        match (markers, remeasure) {
            (true, true) => Measurement::Markdown,
            (true, false) if has_findings() => Measurement::Markdown,
            (_, true) => Measurement::Source,
            _ => Measurement::Saved,
        }
    }

    fn measure(&mut self, pin: &Pin, id: usize, text: &str, (remeasure, test_rules): (bool, bool)) -> io::Result<()> {
        match self.measurement(remeasure) {
            Measurement::Saved => {}
            Measurement::Markdown => {
                let sections = crate::domain::markdown::score_sections(self.path, text);
                let mut markers = self.directives.markers.iter().copied().peekable();
                self.score = self.policy
                    .measure(|| crate::domain::code::score::markdown_sections(text, &sections, |line| {
                        markers.next_if_eq(&line).is_some()
                    }))
                    .map(|score| Cow::Owned(std::sync::Arc::unwrap_or_clone(score)));
            }
            Measurement::Source => {
                self.score = self.policy.measure(|| measure_source(pin, id, text, test_rules))?.map(Cow::Owned);
            }
        }
        Ok(())
    }

    fn apply(&mut self, enabled: u8) {
        let Some(score) = self.score.as_ref() else {
            return;
        };
        self.excluded_lines = self.directives.excluded(score.lines, enabled);
        if self.excluded_lines == score.lines {
            self.excluded_file = true;
            self.score = None;
            return;
        }
        let keep = |f: &Finding| {
            f.value >= self.policy.threshold(f.rule) && !self.directives.suppresses(f.rule, f.line)
        };
        if let Some(first) = score.findings.iter().position(|finding| !keep(finding)) {
            let mut selected: Vec<_> = (0..first).collect();
            selected.extend((first + 1..score.findings.len()).filter(|&i| keep(&score.findings[i])));
            self.selected = Some(selected);
        }
    }

    pub fn lines(&self) -> usize {
        self.score.as_ref().map_or(0, |s| s.lines - self.excluded_lines)
    }

    pub(super) fn findings(&self) -> impl ExactSizeIterator<Item = &Finding> {
        let all = self.score.as_ref().map_or(&[][..], |s| s.findings.as_slice());
        let selected = self.selected.as_deref();
        (0..selected.map_or(all.len(), <[_]>::len))
            .map(move |i| &all[selected.map_or(i, |indices| indices[i])])
    }

    fn has_policy(&self) -> bool {
        self.policy.rules.iter().any(Option::is_some) || !self.directives.spans.is_empty()
    }

    pub fn write_json(&self, out: &mut String) {
        use crate::adapters::query::json::quote_into;
        out.push_str(r#"{"path":"#);
        quote_into(out, self.path);
        out.push_str(r#","score":"#);
        if let Some(value) = self.value {
            write!(out, "{value}").unwrap();
        } else {
            out.push_str("null");
        }
        write!(
            out,
            r#","lines":{},"findings":["#,
            self.lines()
        )
        .unwrap();
        for (i, finding) in self.findings().enumerate() {
            if i != 0 {
                out.push(',');
            }
            out.push_str(r#"{"rule":"#);
            quote_into(out, finding.rule.label());
            write!(out, r#","line":{},"unit":"#, finding.line).unwrap();
            quote_into(out, &finding.unit);
            write!(
                out,
                r#","value":{},"threshold":{}}}"#,
                finding.value,
                self.policy.threshold(finding.rule)
            )
            .unwrap();
        }
        out.push(']');
        if let Some(exclusion) = self.policy.excluded {
            write!(
                out,
                r#","excluded_lines":{},"reason":"#,
                self.excluded_lines
            )
            .unwrap();
            quote_into(out, &exclusion.reason);
            write!(out, r#","config_line":{}"#, exclusion.line).unwrap();
        } else if self.has_policy() {
            write!(
                out,
                r#","excluded_lines":{},"policy":{}"#,
                self.excluded_lines,
                policy_json(&self.policy, &self.directives)
            )
            .unwrap();
        }
        out.push('}');
    }

    fn display_score(&self) -> String {
        let path = self.path;
        match self.value {
            Some(value) => {
                let mut out = format!("{path}: {value:.2}/10 ({} lines)", self.lines());
                findings(&mut out, path, self.findings(), &self.policy);
                out
            }
            _ if self.excluded_file => format!("{path}: excluded ({} lines)", self.excluded_lines),
            _ if self.empty => format!("{path}: unscored (empty)"),
            _ => format!("{path}: unscored (no language profile or non-text)"),
        }
    }

    pub fn display(&self) -> String {
        let path = self.path;
        if let Some(exclusion) = self.policy.excluded {
            return format!(
                "{path}: excluded ({} lines) | .cxconfig:{}: {}",
                self.excluded_lines, exclusion.line, exclusion.reason
            );
        }
        let mut out = self.display_score();
        if self.has_policy() {
            write!(
                out,
                "\npolicy: {}",
                policy_json(&self.policy, &self.directives)
            )
            .unwrap();
        }
        out
    }
}

fn policy_json(policy: &FilePolicy<'_>, directives: &Directives) -> String {
    let config = Rule::ALL.into_iter().filter_map(|rule| {
        let set = policy.rules[rule as usize]?;
        Some(object(&[
            ("rule", quote(rule.id())),
            (
                "threshold",
                set.threshold
                    .map_or_else(|| "null".into(), |n| n.to_string()),
            ),
            ("config_line", set.line.to_string()),
            ("reason", quote(&set.reason)),
        ]))
    });
    let inline = directives.spans.iter().map(|span| {
        object(&[
            ("rule", quote(span.rule.map_or("all", Rule::id))),
            ("directive_line", span.marker.to_string()),
            ("start_line", span.lines.start.to_string()),
            (
                "end_line_exclusive",
                (span.lines.end != usize::MAX)
                    .then(|| span.lines.end.to_string())
                    .unwrap_or_else(|| "null".into()),
            ),
            ("reason", quote(&span.reason)),
        ])
    });
    array(config.chain(inline))
}

fn measure_source(pin: &Pin, id: usize, text: &str, test_rules: bool) -> io::Result<Option<Score>> {
    let entry = &pin.files[id];
    let path = entry.path();
    if let Some(provider) = Language::for_path(path).and_then(|lang| lang.analyzer().configured_score()) {
        let facts = pin.decode_evidence(entry)?;
        return Ok(provider.measure_saved(
            &crate::domain::code::Document { path, text }, &facts, pin.has_compilation_database(), test_rules,
        )
        .map(std::sync::Arc::unwrap_or_clone));
    }
    Ok(
        crate::domain::code::score::source(path, text, None, test_rules)
            .map(std::sync::Arc::unwrap_or_clone),
    )
}

fn findings<'a>(output: &mut String, path: &str, selected: impl Iterator<Item = &'a Finding>, policy: &FilePolicy<'_>) {
    for f in selected {
        write!(
            output,
            "\n{path}:{}: {} | {} {} (threshold {})",
            f.line,
            f.unit.escape_default(),
            f.rule.label(),
            f.value,
            policy.threshold(f.rule)
        )
        .unwrap();
    }
}
