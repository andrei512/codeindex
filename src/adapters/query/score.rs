mod roles;
pub(super) use roles::{Request as RoleRequest, query as roles};
mod file;
use super::{
    Outcome, Pin, ScoreOptions,
    json::{array, object, quote},
    relationships,
};
use crate::domain::code::Language;
use crate::domain::code::score::{
    Rule, Score,
    policy::{FilePolicy, Project, directives::Directives},
};
use std::{fmt::Write, io, path::Path};

pub(super) struct Request<'a> {
    pub path: Option<&'a str>,
    pub options: ScoreOptions,
}

pub(super) fn query(pin: &Pin, cwd: &Path, request: Request<'_>, outcome: &mut Outcome) -> io::Result<String> {
    let scope = query_scope(&pin.root, cwd, request.path)?;
    let project = read_policy(&pin.root, request.options.project_policy)?;
    Report { scope: &scope, options: request.options, project: &project }
        .write(pin, scoped_files(pin, &scope), outcome)
}

pub(super) fn plain(cwd: &Path, path: Option<&str>, options: ScoreOptions, outcome: &mut Outcome) -> io::Result<String> {
    use crate::application::index::build::MemoryQuery;
    let scope = query_scope(cwd, cwd, path)?;
    let policy = read_policy(cwd, options.project_policy)?;
    let needs = MemoryQuery::Score { scope: &scope, test_rules: options.test_rules, inline: options.project_policy, policy: &policy };
    let pin = Pin::memory(cwd, needs)?;
    Report { scope: &scope, options, project: &policy }.write(&pin, 0..pin.files.len(), outcome)
}

fn query_scope(root: &Path, cwd: &Path, path: Option<&str>) -> io::Result<String> {
    Ok(path
        .map(|p| relationships::relative(root, cwd, p))
        .transpose()?
        .unwrap_or_default())
}

fn scoped_files(pin: &Pin, scope: &str) -> std::ops::Range<usize> {
    crate::application::index::scope_range(&pin.files, scope, crate::application::index::Entry::path)
}

#[derive(Default)]
struct Totals {
    indexed: usize,
    scored: usize,
    lines: usize,
    weighted: f64,
    excluded_files: usize,
    excluded_lines: usize,
}

impl Totals {
    fn add(&mut self, file: &file::FileScore<'_>) {
        self.indexed += 1;
        self.excluded_lines += file.excluded_lines;
        self.excluded_files += usize::from(file.excluded_file);
        if let Some(value) = file.value {
            self.scored += 1;
            self.lines += file.lines();
            self.weighted += value * file.lines() as f64;
        }
    }

    fn average(&self) -> Option<f64> {
        (self.lines != 0).then(|| self.weighted / self.lines as f64)
    }

    fn finish_json(&self, mut rows: String) -> String {
        let value = self
            .average()
            .map_or_else(|| "null".into(), |v| v.to_string());
        let header = format!(
            r#"{{"score":{value},"scored_files":{},"indexed_files":{},"lines":{},"files":["#,
            self.scored, self.indexed, self.lines
        );
        rows.insert_str(0, &header);
        rows.push(']');
        if self.excluded_files > 0 || self.excluded_lines > 0 {
            write!(
                rows,
                r#","excluded_files":{},"excluded_lines":{}"#,
                self.excluded_files, self.excluded_lines
            )
            .unwrap();
        }
        rows.push('}');
        rows
    }

    fn summary(&self) -> String {
        let mut output = match self.average() {
            Some(value) => format!(
                "{value:.2}/10 | {}/{} indexed files scored | {} lines",
                self.scored, self.indexed, self.lines
            ),
            None => format!(
                "Unscored: 0/{} indexed files have scorable lines",
                self.indexed
            ),
        };
        if self.excluded_files > 0 || self.excluded_lines > 0 {
            write!(
                output,
                " | {} files excluded ({} lines)",
                self.excluded_files, self.excluded_lines
            )
            .unwrap();
        }
        output
    }
}

// Equal scores keep catalog order, so truncation always retains the earliest paths.
#[derive(Default)]
struct WorstFiles<'a>(Vec<(f64, &'a str, usize)>);

impl<'a> WorstFiles<'a> {
    fn consider(&mut self, path: &'a str, file: &file::FileScore<'_>) {
        const LIMIT: usize = 10;
        let Some(value) = file.value else { return };
        let count = file.findings().len();
        if count == 0 { return; }
        let position = self.0.partition_point(|row| row.0 <= value);
        if position == LIMIT { return; }
        if self.0.len() == LIMIT { self.0.pop(); }
        self.0.insert(position, (value, path, count));
    }
}

struct Report<'a> {
    scope: &'a str,
    options: ScoreOptions,
    project: &'a Project,
}

enum Rows<'a> {
    Json(String),
    Text(WorstFiles<'a>),
}

impl<'a> Rows<'a> {
    fn add(&mut self, count: usize, path: &'a str, file: &file::FileScore<'_>) {
        match self {
            Self::Json(rows) => {
                if count > 1 {
                    rows.push(',');
                }
                file.write_json(rows);
            }
            Self::Text(worst) => worst.consider(path, file),
        }
    }

    fn finish(self, totals: Totals) -> String {
        match self {
            Self::Json(rows) => totals.finish_json(rows),
            Self::Text(_) if totals.indexed == 0 => "No indexed files in scope".into(),
            Self::Text(worst) => worst.summary(totals),
        }
    }
}

impl Report<'_> {
    fn write(self, pin: &Pin, files: std::ops::Range<usize>, outcome: &mut Outcome) -> io::Result<String> {
        let mut totals = Totals::default();
        let mut rows = if self.options.json { Rows::Json(String::new()) } else { Rows::Text(WorstFiles::default()) };
        let mut scratch = crate::domain::pattern::Scratch::default();
        for id in files {
            let path = pin.files[id].path();
            let policy = self.project.resolve_with(path, &mut scratch);
            let file = file::FileScore::read(pin, id, policy, self.options)?;
            if path == self.scope && !self.options.json {
                return Ok(file.display());
            }
            totals.add(&file);
            rows.add(totals.indexed, path, &file);
        }
        if totals.indexed == 0 {
            outcome.code = 1;
        }
        Ok(rows.finish(totals))
    }
}

impl WorstFiles<'_> {
    fn summary(self, totals: Totals) -> String {
        let mut output = totals.summary();
        for (value, path, count) in self.0 {
            write!(output, "\n{path}: {value:.2}/10 ({count} findings)").unwrap();
        }
        output
    }
}

pub(super) fn read_policy(root: &Path, enabled: bool) -> io::Result<Project> {
    if !enabled {
        return Ok(Project::default());
    }
    match std::fs::read_to_string(root.join(".cxconfig")) {
        Ok(text) => Project::parse(&text).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Project::default()),
        Err(error) => Err(io::Error::new(error.kind(), format!(".cxconfig: {error}"))),
    }
}
