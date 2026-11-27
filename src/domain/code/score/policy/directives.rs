//! A directive occupies an entire comment on its own line. The owning location
//! of a finding is the line reported by cx (the declaration for unit metrics).
use super::{MARKER, Rule};
use crate::domain::{
    code::{Language, comment_evidence::CommentEvidence},
    markdown,
};
use std::ops::Range;

#[derive(Debug)]
pub(crate) struct Suppression {
    pub rule: Option<Rule>,
    pub reason: String,
    pub marker: usize,
    pub lines: Range<usize>,
}

#[derive(Default)]
pub(crate) struct Directives {
    pub spans: Vec<Suppression>,
    pub markers: Vec<usize>,
    excluded_lines: usize,
    whole_file: bool,
    masks: RuleMasks,
}

impl Directives {
    pub fn read(path: &str, text: &str, enabled: u8) -> Result<Self, String> {
        let Some(mut source) = SourceComments::for_source(path, text) else {
            return Ok(Self::default());
        };
        let mut directives = Self::default();
        let mut stack = Vec::new();
        let mut offset = 0;
        for (row, line) in text.split_inclusive('\n').enumerate() {
            let start = offset;
            offset += line.len();
            let Some(body) = source.directive(text, start, line) else {
                continue;
            };
            directives
                .apply(body, row + 1, &mut stack)
                .map_err(|why| format!("{path}:{}: {why}", row + 1))?;
            directives.markers.push(row + 1);
        }
        if let Some(&(id, _)) = stack.last() {
            return Err(format!(
                "{path}:{}: unclosed cx-score begin",
                directives.spans[id].marker
            ));
        }
        directives.excluded_lines = source.excluded_lines(&directives, text, enabled);
        Ok(directives)
    }

    pub fn excluded(&self, scored_lines: usize, enabled: u8) -> usize {
        if self.whole_file || enabled == 0 {
            scored_lines
        } else {
            self.excluded_lines.min(scored_lines)
        }
    }

    fn count_excluded(&self, lines: impl Iterator<Item = usize>, enabled: u8) -> usize {
        if self.masks.file & enabled == enabled {
            return lines.count();
        }
        let mut changes = self.masks.changes.iter().copied().peekable();
        let mut active = self.masks.file;
        let mut marker = self.markers.iter().copied().peekable();
        lines
            .filter(|&line| {
                while marker.next_if(|&row| row < line).is_some() {}
                if marker.next_if_eq(&line).is_some() {
                    return true;
                }
                while let Some((_, region)) = changes.next_if(|(start, _)| *start <= line) {
                    active = self.masks.file | region;
                }
                active & enabled == enabled
            })
            .count()
    }

    fn has_line_exclusions(&self, enabled: u8) -> bool {
        !self.whole_file && self.has_active_markers(enabled)
    }

    fn has_active_markers(&self, enabled: u8) -> bool {
        enabled != 0 && !self.markers.is_empty()
    }

    pub fn suppresses(&self, rule: Rule, line: usize) -> bool {
        self.masks.at(line) & (1 << rule as u8) != 0
    }
}

enum SourceComments {
    Markdown(markdown::CommentLines),
    Code {
        lang: Language,
        lexical: Option<CommentEvidence>,
    },
}

impl SourceComments {
    fn excluded_lines(self, policy: &Directives, text: &str, enabled: u8) -> usize {
        if !policy.has_line_exclusions(enabled) {
            return 0;
        }
        match self {
            Self::Markdown(_) => policy.count_excluded(
                text.lines()
                    .enumerate()
                    .filter_map(|(i, line)| (!line.trim().is_empty()).then_some(i + 1)),
                enabled,
            ),
            Self::Code { lexical, .. } => policy.count_excluded(
                lexical.into_iter().flat_map(CommentEvidence::into_lines),
                enabled,
            ),
        }
    }

    fn for_source(path: &str, text: &str) -> Option<Self> {
        if !text.contains(MARKER) {
            return None;
        }
        if markdown::is_markdown(path) {
            return Some(Self::Markdown(markdown::CommentLines::default()));
        }
        let lang =
            Language::for_path(path).or_else(|| path.ends_with(".t").then_some(Language::Perl))?;
        Some(Self::Code {
            lang,
            lexical: None,
        })
    }

    fn directive<'a>(&mut self, text: &str, start: usize, line: &'a str) -> Option<&'a str> {
        if let Self::Markdown(lines) = self
            && !lines.allows_comment(line)
        {
            return None;
        }
        let trimmed = line.trim_start();
        let (prefix, body) = trimmed.split_once(MARKER)?;
        let prefix = prefix.trim_end();
        let body = body.trim();
        match self {
            Self::Markdown(_) => html_comment(prefix, body),
            Self::Code { lang, lexical } => {
                let body = comment_body(*lang, prefix, body)?;
                // Only plausible directive lines require the native comment pass.
                let at = start + (line.len() - trimmed.len());
                let comments =
                    lexical.get_or_insert_with(|| lang.analyzer().comment_evidence(*lang, text));
                comments.allows(at).then_some(body)
            }
        }
    }
}

/// Events count overlapping suppressions independently. Two different
/// directives may jointly disable the last remaining rules for a region.
/// Begin/end directives are properly nested and arrive in source order. Each
/// boundary carries the union of its enclosing regions. File rules apply even
/// to findings that precede the directive.
#[derive(Default)]
struct RuleMasks {
    file: u8,
    changes: Vec<(usize, u8)>,
}

impl RuleMasks {
    fn at(&self, line: usize) -> u8 {
        let end = self.changes.partition_point(|(start, _)| *start <= line);
        self.file | self.changes[..end].last().map_or(0, |&(_, mask)| mask)
    }

    fn boundary(&mut self, line: usize, active: u8) {
        if self.changes.last().is_some_and(|&(start, _)| start == line) {
            self.changes.pop();
        }
        if self.changes.last().map_or(0, |&(_, mask)| mask) != active {
            self.changes.push((line, active));
        }
    }
}

enum Directive<'a> {
    End,
    IgnoreFile(&'a str),
    Begin(&'a str),
}

fn parse_directive(body: &str) -> Result<(Directive<'_>, Option<Rule>), &'static str> {
    let (command, reason) = body
        .split_once(" -- ")
        .map_or((body, ""), |(c, r)| (c, r.trim()));
    let mut words = command.split_whitespace();
    let op = words.next().ok_or("expected ignore-file, begin or end")?;
    let rule = match words.next() {
        Some("all") => None,
        Some(id) => Some(Rule::named(id).ok_or("unknown scoring rule")?),
        None => return Err("expected a rule name or all"),
    };
    if words.next().is_some() {
        return Err("unexpected directive operand");
    }
    Ok((Directive::from_parts(op, reason)?, rule))
}

impl<'a> Directive<'a> {
    fn from_parts(op: &str, reason: &'a str) -> Result<Self, &'static str> {
        Ok(match (op, reason) {
            ("end", "") => Directive::End,
            ("end", _) => return Err("end takes only a rule"),
            (_, "") => return Err("expected -- followed by a reason"),
            ("begin", reason) => Directive::Begin(reason),
            ("ignore-file", reason) => Directive::IgnoreFile(reason),
            _ => return Err("expected ignore-file, begin or end"),
        })
    }
}

impl Directives {
    fn apply(
        &mut self,
        body: &str,
        line: usize,
        stack: &mut Vec<(usize, u8)>,
    ) -> Result<(), &'static str> {
        let (directive, rule) = parse_directive(body)?;
        let mask = rule.map_or(u8::MAX, |r| 1 << r as u8);
        let (start, reason) = match directive {
            Directive::End => return self.close_region(rule, line, stack),
            Directive::IgnoreFile(reason) => {
                self.masks.file |= mask;
                self.whole_file |= rule.is_none();
                (1, reason)
            }
            Directive::Begin(reason) => {
                let active = mask | stack.last().map_or(0, |&(_, outer)| outer);
                stack.push((self.spans.len(), active));
                self.masks.boundary(line + 1, active);
                (line + 1, reason)
            }
        };
        self.spans.push(Suppression {
            rule,
            reason: reason.into(),
            marker: line,
            lines: start..usize::MAX,
        });
        Ok(())
    }

    fn close_region(
        &mut self,
        rule: Option<Rule>,
        line: usize,
        stack: &mut Vec<(usize, u8)>,
    ) -> Result<(), &'static str> {
        let &(id, _) = stack.last().ok_or("end without begin")?;
        if self.spans[id].rule != rule {
            return Err("end must match the innermost begin");
        }
        stack.pop();
        self.spans[id].lines.end = line;
        self.masks
            .boundary(line, stack.last().map_or(0, |&(_, mask)| mask));
        Ok(())
    }
}

fn html_comment<'a>(prefix: &str, body: &'a str) -> Option<&'a str> {
    (prefix == "<!--")
        .then(|| body.strip_suffix("-->"))?
        .map(str::trim_end)
}

fn comment_body<'a>(lang: Language, prefix: &str, body: &'a str) -> Option<&'a str> {
    if directive_line_prefix(lang, prefix) {
        return Some(body);
    }
    body.strip_suffix(comment_suffix(lang, prefix)?)
        .map(str::trim_end)
}

fn comment_suffix(lang: Language, prefix: &str) -> Option<&'static str> {
    match (lang, prefix) {
        (Language::Html, "<!--") => Some("-->"),
        (Language::Powershell, "<#") => Some("#>"),
        (_, "/*") if lang.block_comments() => Some("*/"),
        _ => None,
    }
}

fn directive_line_prefix(lang: Language, prefix: &str) -> bool {
    use Language::*;
    match (lang, prefix) {
        (Mako, "##") | (Assembly, ";" | "#") | (Procfile, "#") => true,
        (Batchfile, x) if x == "::" || x.eq_ignore_ascii_case("rem") => true,
        (_, "//" | "#" | "--") => lang.line_comment(prefix),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_comment_examples_follow_native_block_boundaries() {
        let marker = "<!-- cx-score: ignore-file all -- generated -->";
        for (open, close) in [
            ("<PRE>", "</pRe>"),
            ("<script>", "value;</SCRIPT>"),
            ("<!-- outer", "-->"),
            ("<![CDATA[", "]]>"),
            ("- ```md", "  ```"),
        ] {
            let source = format!("{open}\n  {marker}\n{close}\n{marker}\n");
            let found = Directives::read("README.md", &source, u8::MAX).unwrap();
            assert_eq!(found.markers, [4], "{open}");
        }
        for open in ["```invalid`info", "<div>"] {
            let source = format!("{open}\n{marker}\n");
            assert_eq!(
                Directives::read("README.md", &source, u8::MAX)
                    .unwrap()
                    .markers,
                [2]
            );
        }
    }

    #[test]
    fn a_native_directive_for_each_scored_profile() {
        let profiles = [
            ("x.rs", "//", ""),
            ("x.ts", "//", ""),
            ("x.py", "#", ""),
            ("x.bzl", "#", ""),
            ("x.sh", "#", ""),
            ("x.ps1", "#", ""),
            ("x.cs", "//", ""),
            ("x.swift", "//", ""),
            ("x.c", "//", ""),
            ("x.cpp", "//", ""),
            ("x.proto", "//", ""),
            ("x.css", "/*", "*/"),
            ("x.html", "<!--", "-->"),
            ("x.sql", "--", ""),
            ("x.go", "//", ""),
            ("x.java", "//", ""),
            ("x.kt", "//", ""),
            ("x.m", "//", ""),
            ("x.dart", "//", ""),
            ("x.php", "//", ""),
            ("x.hack", "//", ""),
            ("Dockerfile", "#", ""),
            ("Makefile", "#", ""),
            ("x.cmd", "REM", ""),
            ("CMakeLists.txt", "#", ""),
            ("Procfile", "#", ""),
            ("x.hcl", "#", ""),
            ("x.nix", "#", ""),
            ("x.rb", "#", ""),
            ("x.lua", "--", ""),
            ("x.pl", "#", ""),
            ("x.asm", ";", ""),
            ("x.mako", "##", ""),
            ("x.plpgsql", "--", ""),
            ("x.md", "<!--", "-->"),
        ];
        for (path, open, close) in profiles {
            let header = match path {
                "x.php" => "<?php\n",
                "x.hack" => "<?hh\n",
                _ => "",
            };
            let text = format!(
                "{header}{open} cx-score: begin depth -- framework {close}\nbody\n{open} cx-score: end depth {close}\n"
            );
            let rules = Directives::read(path, &text, u8::MAX).unwrap();
            let row = 2 + usize::from(!header.is_empty());
            assert!(rules.suppresses(Rule::Nesting, row), "{path}");
            assert!(!rules.suppresses(Rule::Complexity, row), "{path}");
            assert!(!rules.suppresses(Rule::Nesting, row + 1), "{path}");
        }
    }

    #[test]
    fn literal_bodies_and_embedded_examples_are_inert() {
        for (path, open, marker, close) in [
            ("x.rs", "let value = r###\"", "//", "\"###;"),
            ("x.py", "s = \"\"\"", "#", "\"\"\""),
            ("x.go", "const example = `", "//", "`"),
            ("x.js", "const example = `", "//", "`;"),
            ("x.ps1", "$s = @'", "#", "'@"),
            ("x.cmake", "set(s [=[", "#", "]=])"),
            ("x.rb", "s = <<~BODY", "#", "BODY"),
            ("x.pl", "my $s = <<'BODY';", "#", "BODY"),
            ("x.lua", "s = [==[", "--", "]==]"),
            ("x.php", "<?php\n$s = <<<'END'", "//", "END;"),
            ("x.tf", "s = <<-BODY", "#", "BODY"),
            ("x.nix", "s = ''", "#", "'';"),
            ("x.sh", "cat <<'BODY'", "#", "BODY"),
            ("x.sql", "SELECT $example$", "--", "$example$;"),
            ("Dockerfile", "FROM scratch\nCOPY <<END /data", "#", "END"),
            ("Makefile", "define TEMPLATE", "#", "endef"),
            ("x.mako", "<%text>", "##", "</%text>"),
        ] {
            let text = format!("{open}\n{marker} cx-score: ignore-file all -- example\n{close}\n");
            assert!(
                Directives::read(path, &text, u8::MAX)
                    .unwrap()
                    .spans
                    .is_empty(),
                "{path}"
            );
        }
    }

    #[test]
    fn markdown_examples_contain_complete_but_inert_directives() {
        let marker = "<!-- cx-score: ignore-file all -- example -->";
        for text in [format!("```md\n{marker}\n```"), format!("    {marker}")] {
            assert!(
                Directives::read("x.md", &text, u8::MAX)
                    .unwrap()
                    .spans
                    .is_empty()
            );
        }
    }

    #[test]
    fn html_raw_text_does_not_supply_comment_directives() {
        let tag = "<!-- cx-score: ignore-file all -- example -->";
        let text = format!(
            "<textarea>\n{tag}\n</textarea>\n<script>const s = `\n{tag}\n`;</script>\n{tag}"
        );
        let rules = Directives::read("x.html", &text, u8::MAX).unwrap();
        assert_eq!(
            rules.spans.iter().map(|s| s.marker).collect::<Vec<_>>(),
            [7]
        );
    }

    #[test]
    fn sql_regions_use_procedural_lines_and_ignore_literal_examples() {
        let sql = "SELECT $$\n-- cx-score: ignore-file all -- example\n$$;\nCREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $body$\nBEGIN\n-- cx-score: begin all -- measured\nPERFORM 1;\nPERFORM 2;\n-- cx-score: end all\nRETURN 0;\nEND;\n$body$;\n";
        let rules = Directives::read("x.sql", sql, u8::MAX).unwrap();
        assert_eq!(rules.spans.len(), 1);
        assert_eq!(rules.excluded_lines, 2);
        assert_eq!(rules.spans[0].lines, 7..9);
    }

    #[test]
    fn finding_lookup_agrees_with_reported_scopes_at_every_boundary() {
        let source = "fn a() {}
// cx-score: begin depth -- outer
// cx-score: begin all -- inner
fn b() {}
// cx-score: end all
// cx-score: begin depth -- empty
// cx-score: end depth
fn c() {}
// cx-score: end depth
// cx-score: ignore-file complexity -- file
fn d() {}";
        let directives = Directives::read("cases.rs", source, u8::MAX).unwrap();
        for line in 1..=source.lines().count() {
            for rule in Rule::ALL {
                let spelled = directives
                    .spans
                    .iter()
                    .any(|span| span.lines.contains(&line) && span.rule.is_none_or(|r| r == rule));
                assert_eq!(
                    directives.suppresses(rule, line),
                    spelled,
                    "{rule:?} at {line}"
                );
            }
        }
        assert_eq!(directives.excluded_lines, 1);
    }

    #[test]
    fn errors_are_local_and_ranges_do_not_double_count() {
        let text = "// cx-score: begin all -- outer\nfn f() {\n// cx-score: begin all -- inner\nf();\n// cx-score: end all\n}\n// cx-score: end all\n";
        assert_eq!(
            Directives::read("a.rs", text, u8::MAX)
                .unwrap()
                .excluded_lines,
            3
        );
        for text in [
            "// cx-score: begin depth -- x",
            "// cx-score: ignore-file all",
            "// cx-score: end all",
        ] {
            assert!(
                Directives::read("a.rs", text, u8::MAX)
                    .err()
                    .unwrap()
                    .starts_with("a.rs:1:")
            );
        }
    }
}
