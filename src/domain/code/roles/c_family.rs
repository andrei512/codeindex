//! Follow simple function-like macro aliases in the source and its quoted local
//! headers. Header bodies stay in the pinned snapshot and never execute.
use super::{Language, Role, Roles, Syntax, Token, lexer, script};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Component, Path, PathBuf},
};

#[derive(Default)]
struct Defines {
    local: HashSet<String>,
    aliases: Vec<(String, String)>,
}

fn definitions(t: &[Token], pairs: &[usize]) -> Defines {
    let mut d = Defines::default();
    for (i, w) in t.iter().enumerate() {
        if w.text != "#" || i > 0 && t[i - 1].line == w.line {
            continue;
        }
        let Some([op, name, open]) = t.get(i + 1..i + 4) else {
            continue;
        };
        if op.text != "define"
            || name.line != w.line
            || open.text != "("
            || open.span.start != name.span.end
        {
            continue;
        }
        d.local.insert(name.text.to_owned());
        let close = pairs[i + 3];
        if close >= t.len() {
            continue;
        }
        let mut j = close + 1;
        while t.get(j).is_some_and(|v| v.text == "\\") {
            j += 1;
        }
        if let Some(replacement) = t.get(j)
            && (replacement.line == t[close].line || j > close + 1)
            && t.get(j + 1).is_some_and(|v| v.text == "(")
        {
            d.aliases
                .push((name.text.to_owned(), replacement.text.to_owned()));
        }
    }
    d
}

fn builtin(name: &str) -> bool {
    matches!(
        name,
        "TEST"
            | "TEST_F"
            | "TEST_P"
            | "TYPED_TEST"
            | "TEST_CASE"
            | "SCENARIO"
            | "TEST_IMPL"
            | "BOOST_AUTO_TEST_CASE"
            | "BOOST_FIXTURE_TEST_CASE"
            | "START_TEST"
    )
}
#[derive(Default)]
pub(crate) struct Headers(HashMap<PathBuf, Defines>);
impl Headers {
    pub(super) fn collect(
        &mut self,
        path: &str,
        s: &Syntax,
        res: &mut Roles,
        mut read: impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<()> {
        let local = definitions(s.tokens, s.pairs);
        let mut aliases: Vec<_> = local.aliases.clone();
        let mut headers = HashSet::new();
        for i in 0..s.tokens.len() {
            if !s.seq(i, &["#", "include"]) || !s.string(i + 2) {
                continue;
            }
            let Some(rel) = s
                .w(i + 2)
                .strip_prefix('"')
                .and_then(|r| r.strip_suffix('"'))
            else {
                continue;
            };
            if Path::new(rel).is_absolute() {
                continue;
            }
            let mut key = PathBuf::new();
            for c in Path::new(path)
                .parent()
                .unwrap_or(Path::new(""))
                .join(rel)
                .components()
            {
                match c {
                    Component::Normal(p) => key.push(p),
                    Component::ParentDir => {
                        if !key.pop() {
                            key.clear();
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if key.as_os_str().is_empty() || !headers.insert(key.clone()) {
                continue;
            }
            if !self.0.contains_key(&key) {
                let defs = if let Some(text) = read(&key)? {
                    let tokens = lexer::tokens(&text, Language::Cpp);
                    let pairs = script::paired_for_score(&tokens);
                    definitions(&tokens, &pairs)
                } else {
                    Defines::default()
                };
                self.0.insert(key.clone(), defs);
            }
            aliases.extend(self.0[&key].aliases.iter().cloned());
        }
        let mut resolved = HashSet::new();
        loop {
            let count = resolved.len();
            for (a, b) in &aliases {
                if builtin(b) && !local.local.contains(b) || resolved.contains(b.as_str()) {
                    resolved.insert(a.as_str());
                }
            }
            if resolved.len() == count {
                break;
            }
        }
        for (i, t) in s.tokens.iter().enumerate() {
            if !(builtin(t.text) && !local.local.contains(t.text) || resolved.contains(t.text))
                || i > 0 && s.w(i - 1) == "define"
                || s.w(i + 1) != "("
            {
                continue;
            }
            if let Some(close) = s.close(i + 1)
                && s.w(close + 1) == "{"
            {
                s.add(
                    res,
                    i,
                    s.close(close + 1),
                    Role::Test,
                    "test registration macro",
                );
            }
        }
        Ok(())
    }
}
