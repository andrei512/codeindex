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
        let score = f.score.as_deref().filter(|s| s.lines > 0);
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
                worst.push((value, f.path(), s));
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
    for (value, path, s) in worst.into_iter().take(10) {
        write!(
            output,
            "\n{path}: {value:.2}/10 ({} findings)",
            s.findings.len()
        )
        .unwrap();
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
