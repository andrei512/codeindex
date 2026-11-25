use super::{
    Outcome, Pin,
    json::{array, object, quote},
    relationships,
};
use crate::domain::code::score::Score;
use std::{fmt::Write, io, path::Path};

pub(super) fn query(
    pin: &Pin,
    cwd: &Path,
    path: Option<&str>,
    json: bool,
    test_rules: bool,
    outcome: &mut Outcome,
) -> io::Result<String> {
    let scope = path
        .map(|p| relationships::relative(&pin.root, cwd, p))
        .transpose()?
        .unwrap_or_default();
    let mut included = 0;
    let mut supported = 0;
    let mut loc = 0;
    let mut weighted = 0.;
    let mut worst = Vec::new();
    let mut rows = Vec::new();
    let first = pin.files.partition_point(|f| f.path() < scope.as_str());
    for f in pin.files[first..]
        .iter()
        .take_while(|f| f.path().starts_with(&scope))
    {
        if !Path::new(f.path()).starts_with(&scope) {
            continue;
        }
        included += 1;
        let single = f.path() == scope && !json;
        let score = if test_rules {
            f.test_score.as_deref()
        } else {
            None
        }
        .or(f.score.as_deref())
        .filter(|s| s.lines > 0);
        let value = score.map(Score::value);
        if json {
            rows.push(object(&[
                ("path", quote(f.path())),
                (
                    "score",
                    value.map_or_else(|| "null".into(), |v| v.to_string()),
                ),
                ("lines", score.map_or(0, |s| s.lines).to_string()),
                (
                    "findings",
                    array(score.into_iter().flat_map(|s| s.findings.iter()).map(|f| {
                        object(&[
                            ("rule", quote(f.rule.label())),
                            ("line", f.line.to_string()),
                            ("unit", quote(&f.unit)),
                            ("value", f.value.to_string()),
                            ("threshold", f.rule.threshold().to_string()),
                        ])
                    })),
                ),
            ]));
        }
        if let (Some(s), Some(value)) = (score, value) {
            if single {
                let mut output = format!("{}: {:.2}/10 ({} lines)", f.path(), value, s.lines);
                findings(&mut output, f.path(), s);
                return Ok(output);
            }
            supported += 1;
            loc += s.lines;
            weighted += value * s.lines as f64;
            if !json && !s.findings.is_empty() {
                worst.push((value, f.path(), s.findings.len()));
            }
        } else if single {
            if f.score.is_some() {
                return Ok(format!("{}: unscored (empty)", f.path()));
            }
            return Ok(format!(
                "{}: unscored (no language profile or non-text)",
                f.path()
            ));
        }
    }
    if json {
        if included == 0 {
            outcome.code = 1;
        }
        return Ok(object(&[
            (
                "score",
                if loc == 0 {
                    "null".into()
                } else {
                    (weighted / loc as f64).to_string()
                },
            ),
            ("scored_files", supported.to_string()),
            ("indexed_files", included.to_string()),
            ("lines", loc.to_string()),
            ("files", array(rows)),
        ]));
    }
    if included == 0 {
        outcome.code = 1;
        return Ok("No indexed files in scope".into());
    }
    if loc == 0 {
        return Ok(format!(
            "Unscored: 0/{included} indexed files have scorable lines"
        ));
    }
    worst.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(b.1)));
    let mut output = format!(
        "{:.2}/10 | {supported}/{included} indexed files scored | {loc} lines",
        weighted / loc as f64
    );
    for (value, path, count) in worst.into_iter().take(10) {
        write!(output, "\n{path}: {value:.2}/10 ({} findings)", count).unwrap();
    }
    Ok(output)
}
fn findings(output: &mut String, path: &str, score: &Score) {
    for f in &score.findings {
        write!(
            output,
            "\n{path}:{}: {} | {} {} (threshold {})",
            f.line,
            f.unit.escape_default(),
            f.rule.label(),
            f.value,
            f.rule.threshold()
        )
        .unwrap();
    }
}

pub(super) fn roles(
    pin: &Pin,
    cwd: &Path,
    path: Option<&str>,
    json: bool,
    outcome: &mut Outcome,
) -> io::Result<String> {
    use crate::domain::code::roles::{Context, classify_with, needs_source};
    let mut context = Context::new(pin.files.iter().map(|f| f.path()));
    let scope = path
        .map(|p| relationships::relative(&pin.root, cwd, p))
        .transpose()?
        .unwrap_or_default();
    let mut rows = Vec::new();
    let mut window = pin.source_window();
    let first = pin.files.partition_point(|f| f.path() < scope.as_str());
    for (i, f) in pin
        .files
        .iter()
        .enumerate()
        .skip(first)
        .take_while(|(_, f)| f.path().starts_with(&scope))
    {
        if !Path::new(f.path()).starts_with(&scope) || f.bytes.is_none() {
            continue;
        }
        let text = if needs_source(f.path()) {
            window.get(i)?
        } else {
            ""
        };
        let roles = classify_with(f.path(), text, &mut context, |header| {
            let Some(header) = header.to_str() else {
                return Ok(None);
            };
            match pin.files.binary_search_by(|f| f.path().cmp(header)) {
                Ok(id) if pin.files[id].bytes.is_some() => pin.source(id).map(Some),
                _ => Ok(None),
            }
        })?;
        if json {
            rows.push(object(&[
                ("path", quote(f.path())),
                ("default_role", quote(roles.base.0.label())),
                ("reason", quote(roles.base.1)),
                (
                    "regions",
                    array(roles.regions.iter().map(|r| {
                        object(&[
                            ("start_line", r.first.to_string()),
                            ("end_line", r.last.to_string()),
                            ("start_byte", r.start.to_string()),
                            ("end_byte", r.end.to_string()),
                            ("role", quote(r.evidence.0.label())),
                            ("reason", quote(r.evidence.1)),
                        ])
                    })),
                ),
            ]));
        } else {
            let mut row = format!("{}: {} ({})", f.path(), roles.base.0.label(), roles.base.1);
            for r in roles.regions {
                write!(
                    row,
                    "\n{}:{}-{}: {} ({})",
                    f.path(),
                    r.first,
                    r.last,
                    r.evidence.0.label(),
                    r.evidence.1
                )
                .unwrap();
            }
            rows.push(row);
        }
    }
    if rows.is_empty() {
        outcome.code = 1;
    }
    Ok(if json {
        object(&[("files", array(rows))])
    } else {
        rows.join("\n")
    })
}
