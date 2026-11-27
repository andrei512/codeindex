//! Resolve explicitly configured TypeScript path substitutions. A nearer
//! tsconfig defines the context; ambiguous wildcard rules are left unresolved.
use super::{Kind, Language, Token, Unit, tok};
use std::collections::HashMap;
use std::path::Path;
pub(super) struct Config {
    base: String,
    base_url: bool,
    routes: Vec<Substitution>,
}
struct Substitution {
    written: String,
    star: Option<usize>,
    destinations: Vec<String>,
}

impl Substitution {
    fn wildcard_match<'a>(&self, name: &'a str) -> Option<(usize, &'a str)> {
        let star = self.star?;
        let middle = name
            .strip_prefix(&self.written[..star])?
            .strip_suffix(&self.written[star + 1..])?;
        Some((star, middle))
    }
}

struct MatchedSubstitution<'config, 'name> {
    middle: &'name str,
    destinations: &'config [String],
}
#[derive(Default)]
pub(super) struct Projects {
    configs: HashMap<String, Config>,
}
fn value(s: &str) -> Option<&str> {
    s.strip_prefix('"')?
        .strip_suffix('"')
        .filter(|s| !s.contains('\\'))
}
fn property(t: &[Token], open: usize, key: &str, end: &[usize]) -> Option<usize> {
    let mut p = open + 1;
    while p < end[open] {
        if value(tok(t, p)) == Some(key) && tok(t, p + 1) == ":" {
            return Some(p + 2);
        }
        p = after_value(t, end, p);
    }
    None
}
fn after_value(tokens: &[Token], pairs: &[usize], at: usize) -> usize {
    match tok(tokens, at) {
        "{" | "[" => pairs[at] + 1,
        _ => at + 1,
    }
}

impl Config {
    fn parse(text: &str, parent: &str) -> Option<Self> {
        let mut config = Config {
            base: parent.to_owned(),
            base_url: false,
            routes: Vec::new(),
        };
        let t = crate::domain::code::lexer::tokens(text, Language::Js);
        let end = super::syntax::paired(&t);
        if tok(&t, 0) != "{" || end[0] >= t.len() {
            return None;
        }
        let options = property(&t, 0, "compilerOptions", &end);
        let Some(opts) = options.filter(|&p| tok(&t, p) == "{") else {
            return Some(config);
        };
        if let Some(url) = property(&t, opts, "baseUrl", &end).and_then(|p| value(tok(&t, p))) {
            let base = crate::domain::code::relative_path(Path::new(parent), url)?;
            config.base_url = true;
            config.base = base.to_string_lossy().into_owned();
        }
        if let Some(p) = property(&t, opts, "paths", &end).filter(|&p| tok(&t, p) == "{") {
            config.routes = path_routes(&t, &end, p);
        }
        Some(config)
    }
}

fn path_routes(tokens: &[Token], pairs: &[usize], open: usize) -> Vec<Substitution> {
    let mut routes = Vec::new();
    let mut at = open + 1;
    while at < pairs[open] {
        let Some((route, after)) = path_route(tokens, pairs, at) else {
            at += 1;
            continue;
        };
        routes.push(route);
        at = after;
    }
    routes
}

fn path_route(tokens: &[Token], pairs: &[usize], at: usize) -> Option<(Substitution, usize)> {
    let written = value(tok(tokens, at))?;
    if (tok(tokens, at + 1), tok(tokens, at + 2)) != (":", "[") {
        return None;
    }
    let stop = pairs[at + 2].min(tokens.len());
    let destinations = tokens[at + 3..stop]
        .iter()
        .filter(|token| token.kind == Kind::String)
        .filter_map(|token| value(token.text))
        .map(str::to_owned)
        .collect();
    let route = Substitution {
        written: written.to_owned(),
        star: written.find('*'),
        destinations,
    };
    Some((route, stop + 1))
}

impl Projects {
    pub fn read(units: &[Unit]) -> Self {
        let mut this = Self::default();
        for u in units.iter().filter(|u| {
            Path::new(u.path)
                .file_name()
                .is_some_and(|s| s == "tsconfig.json")
        }) {
            let parent = Path::new(u.path).parent().unwrap().to_str().unwrap_or("");
            if let Some(config) = Config::parse(u.text, parent) {
                this.configs.insert(parent.to_owned(), config);
            }
        }
        this
    }
    pub(super) fn at(&self, directory: &Path) -> Option<&Config> {
        self.configs.get(directory.to_str()?)
    }
}

impl Config {
    pub(super) fn lookup(&self, name: &str, paths: &HashMap<&str, usize>) -> Option<usize> {
        let base = Path::new(&self.base);
        let lookup =
            |relative: &str| super::resolution::module_in(base, Language::Js, relative, paths);
        if let Some(matched) = self.substitution(name) {
            return matched.destinations.iter().find_map(|destination| {
                let relative = format!("./{}", destination.replace('*', matched.middle));
                lookup(&relative)
            });
        }
        if self.base_url && !name.starts_with(['.', '/']) {
            return lookup(&format!("./{name}"));
        }
        None
    }

    fn substitution<'config, 'name>(
        &'config self,
        name: &'name str,
    ) -> Option<MatchedSubstitution<'config, 'name>> {
        let wildcards = self.routes.iter().filter_map(|route| {
            let (weight, middle) = route.wildcard_match(name)?;
            Some((
                weight,
                MatchedSubstitution {
                    middle,
                    destinations: &route.destinations,
                },
            ))
        });
        if let Some(route) = self.routes.iter().find(|route| route.written == name) {
            return Some(MatchedSubstitution {
                middle: "",
                destinations: &route.destinations,
            });
        }
        // max_by_key keeps the last tie, so inspect the routes in reverse.
        wildcards
            .rev()
            .max_by_key(|(weight, _)| *weight)
            .map(|(_, matched)| matched)
    }
}
