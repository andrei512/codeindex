//! Framework registration rules for methods and Swift top-level tests.
use super::*;
use std::ops::Range;

pub(super) struct Methods<'a> {
    lang: Language,
    syntax: &'a Syntax<'a>,
    annotations: Annotations<'a>,
}

struct Method {
    start: usize,
    name: usize,
    close: usize,
}

struct MethodCursor {
    at: usize,
    start: usize,
    annotation: Option<(usize, Role)>,
}

type Registration = ((usize, Role), &'static str);

fn java_candidate(s: &Syntax, name: usize, annotated: bool, preceding: Option<&str>) -> bool {
    is_word(s, name) && (annotated || preceding == Some("void"))
}

fn java_conventional<'a>(
    s: &Syntax,
    name: usize,
    mut modifiers: impl Iterator<Item = &'a str>,
) -> bool {
    let void = name
        .checked_sub(1)
        .is_some_and(|prior| s.w(prior) == "void");
    void && modifiers.any(|word| word == "public")
}

fn swift_conventional<'a>(
    s: &Syntax,
    close: usize,
    mut modifiers: impl Iterator<Item = &'a str>,
) -> bool {
    !s.seq(close + 1, &["-", ">"])
        && !modifiers.any(|word| matches!(word, "static" | "private" | "fileprivate"))
}

impl<'a> Methods<'a> {
    pub(super) fn new(lang: Language, syntax: &'a Syntax<'a>) -> Self {
        Self {
            lang,
            syntax,
            annotations: Annotations::new(lang, syntax),
        }
    }

    fn annotation(&self, at: usize) -> (usize, Option<Role>) {
        let s = self.syntax;
        let start = at + 1;
        let end = annotation_end(s, start);
        let role = self.annotations.role(self.lang, s, start, end);
        let next = end + 1;
        let after = if s.w(next) == "(" {
            s.close(next).map_or(next, |close| close + 1)
        } else {
            next
        };
        (after, role)
    }

    fn signature(&self, start: usize, name: usize, annotated: bool) -> Option<Method> {
        let syntax = self.syntax;
        let preceding = name.checked_sub(1).map(|index| syntax.w(index));
        let open = self.parameter_start(name, annotated, preceding)?;
        if syntax.w(open) != "(" {
            return None;
        }
        Some(Method {
            start,
            name,
            close: syntax.close(open)?,
        })
    }

    fn parameter_start(
        &self,
        name: usize,
        annotated: bool,
        preceding: Option<&str>,
    ) -> Option<usize> {
        let next = name + 1;
        match (self.lang, preceding) {
            (Language::Java, _) if java_candidate(self.syntax, name, annotated, preceding) => {
                Some(next)
            }
            (Language::Swift, Some("func")) => Some(next),
            (Language::Hack, Some("function")) => self.hack_parameters(next),
            _ => None,
        }
    }

    fn hack_parameters(&self, next: usize) -> Option<usize> {
        if self.syntax.w(next) == "<" {
            return script::angle_end(self.syntax.tokens, next).map(|end| end + 1);
        }
        Some(next)
    }

    fn conventional(&self, method: &Method) -> bool {
        let syntax = self.syntax;
        let Method { start, name, close } = *method;
        if !syntax.w(name).starts_with("test") {
            return false;
        }
        let no_parameters = close == name + 2;
        let mut modifiers = syntax.tokens[start..name].iter().map(|token| token.text);
        match self.lang {
            Language::Java => no_parameters && java_conventional(syntax, name, modifiers),
            Language::Swift => no_parameters && swift_conventional(syntax, close, modifiers),
            Language::Hack => {
                !modifiers.any(|word| matches!(word, "protected" | "private" | "static"))
            }
            _ => false,
        }
    }

    fn registration(
        &self,
        method: &Method,
        suite: bool,
        annotation: Option<(usize, Role)>,
    ) -> Option<Registration> {
        if suite && self.conventional(method) {
            return Some((
                annotation.unwrap_or((method.name, Role::Test)),
                "test-case method",
            ));
        }
        annotation.map(|site| (site, "test framework annotation"))
    }

    pub(super) fn collect(&self, body: Range<usize>, suite: bool, res: &mut Roles) {
        let mut cursor = MethodCursor {
            at: body.start,
            start: body.start,
            annotation: None,
        };
        while cursor.at < body.end {
            self.advance(&mut cursor, suite, res);
        }
    }

    fn advance(&self, cursor: &mut MethodCursor, suite: bool, res: &mut Roles) {
        let s = self.syntax;
        let i = cursor.at;
        if s.w(i) == "@" {
            let (next, role) = self.annotation(i);
            cursor.annotation = role.map(|role| (i, role)).or(cursor.annotation);
            cursor.at = next;
            return;
        }
        if let Some(method) = self.signature(cursor.start, i, cursor.annotation.is_some()) {
            self.register(res, &method, suite, cursor.annotation.take());
        }
        cursor.at = s.close(i).unwrap_or(i) + 1;
        if matches!(s.w(i), "{" | ";") {
            cursor.annotation = None;
            cursor.start = cursor.at;
        }
    }

    fn register(
        &self,
        res: &mut Roles,
        method: &Method,
        suite: bool,
        annotation: Option<(usize, Role)>,
    ) {
        let Some(((site, role), reason)) = self.registration(method, suite, annotation) else {
            return;
        };
        let Some(end) = self.syntax.body(method.close + 1) else {
            return;
        };
        self.syntax.add(res, site, Some(end), role.evidence(reason));
    }
}
