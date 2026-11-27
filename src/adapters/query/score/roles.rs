//! Render file roles and embedded test regions directly into the response buffer.
use super::{Outcome, Pin, query_scope, scoped_files};
use crate::{adapters::query::json::quote_into, domain::code::roles::Roles};
use std::{fmt::Write, io, path::Path};

pub(in crate::adapters::query) struct Request<'a> {
    pub cwd: &'a Path,
    pub path: Option<&'a str>,
    pub json: bool,
}

pub(in crate::adapters::query) fn query(
    pin: &Pin,
    request: Request<'_>,
    outcome: &mut Outcome,
) -> io::Result<String> {
    use crate::domain::code::roles::{Context, classify_with, needs_source};
    let mut context = Context::new(pin.files.iter().map(|file| file.path()));
    let scope = query_scope(&pin.root, request.cwd, request.path)?;
    let mut output = Output::new(request.json);
    let mut window = pin.source_window();
    for id in scoped_files(pin, &scope).filter(|&id| pin.files[id].bytes.is_some()) {
        let path = pin.files[id].path();
        let text = needs_source(path).then(|| window.get(id)).transpose()?.unwrap_or_default();
        let roles = classify_with(path, text, &mut context, |header| read_header(pin, header))?;
        output.row(path, &roles);
    }
    Ok(output.finish(outcome))
}

fn read_header(pin: &Pin, header: &Path) -> io::Result<Option<String>> {
    let Some(header) = header.to_str() else { return Ok(None) };
    match pin.files.binary_search_by(|file| file.path().cmp(header)) {
        Ok(id) if pin.files[id].bytes.is_some() => pin.source(id).map(Some),
        _ => Ok(None),
    }
}

struct Output {
    text: String,
    separator: char,
    row: fn(&mut String, &str, &Roles),
    end: &'static str,
    empty: bool,
}

impl Output {
    fn new(json: bool) -> Self {
        if json {
            Self { text: r#"{"files":["#.into(), separator: ',', row: write_json, end: "]}", empty: true }
        } else {
            Self { text: String::new(), separator: '\n', row: write_text, end: "", empty: true }
        }
    }

    fn row(&mut self, path: &str, roles: &Roles) {
        if !self.empty {
            self.text.push(self.separator);
        }
        self.empty = false;
        (self.row)(&mut self.text, path, roles);
    }

    fn finish(mut self, outcome: &mut Outcome) -> String {
        if self.empty {
            outcome.code = 1;
        }
        self.text.push_str(self.end);
        self.text
    }
}

fn write_json(out: &mut String, path: &str, roles: &Roles) {
    out.push_str(r#"{"path":"#);
    quote_into(out, path);
    out.push_str(r#","default_role":"#);
    quote_into(out, roles.base.0.label());
    out.push_str(r#","reason":"#);
    quote_into(out, roles.base.1);
    out.push_str(r#","regions":["#);
    for (i, region) in roles.regions.iter().enumerate() {
        if i != 0 {
            out.push(',');
        }
        write!(
            out,
            r#"{{"start_line":{},"end_line":{},"start_byte":{},"end_byte":{},"role":"#,
            region.first, region.last, region.start, region.end
        )
        .unwrap();
        quote_into(out, region.evidence.0.label());
        out.push_str(r#","reason":"#);
        quote_into(out, region.evidence.1);
        out.push('}');
    }
    out.push_str("]}");
}

fn write_text(out: &mut String, path: &str, roles: &Roles) {
    write!(
        out,
        "{}: {} ({})",
        path,
        roles.base.0.label(),
        roles.base.1
    )
    .unwrap();
    for r in &roles.regions {
        write!(
            out,
            "\n{}:{}-{}: {} ({})",
            path,
            r.first,
            r.last,
            r.evidence.0.label(),
            r.evidence.1
        )
        .unwrap();
    }
}
