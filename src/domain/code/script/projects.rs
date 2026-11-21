//! Resolve explicitly configured TypeScript path substitutions. A nearer
//! tsconfig defines the context; ambiguous wildcard rules are left unresolved.
use super::{Kind, Language, Token, Unit, tok};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
struct Config {
    base: String,
    base_url: bool,
    routes: Vec<(String, Vec<String>)>,
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
        if matches!(tok(t, p), "{" | "[") {
            p = end[p];
        }
        p += 1;
    }
    None
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
            let mut config = Config {
                base: parent.to_owned(),
                base_url: false,
                routes: Vec::new(),
            };
            let t = crate::domain::code::lexer::tokens(u.text, Language::Js);
            let end = super::syntax::paired(&t);
            if tok(&t, 0) != "{" || end[0] >= t.len() {
                continue;
            }
            let options = property(&t, 0, "compilerOptions", &end);
            if let Some(opts) = options.filter(|&p| tok(&t, p) == "{") {
                if let Some(url) =
                    property(&t, opts, "baseUrl", &end).and_then(|p| value(tok(&t, p)))
                {
                    let mut base = PathBuf::from(parent);
                    let mut inside = true;
                    for part in Path::new(url).components() {
                        match part {
                            Component::Normal(s) => base.push(s),
                            Component::CurDir => {}
                            Component::ParentDir if base.pop() => {}
                            _ => {
                                inside = false;
                                break;
                            }
                        }
                    }
                    if !inside {
                        continue;
                    }
                    config.base_url = true;
                    config.base = base.to_string_lossy().into_owned();
                }
                if let Some(p) = property(&t, opts, "paths", &end).filter(|&p| tok(&t, p) == "{") {
                    let mut j = p + 1;
                    while j < end[p] {
                        if let Some(key) = value(tok(&t, j))
                            && tok(&t, j + 1) == ":"
                            && tok(&t, j + 2) == "["
                        {
                            let stop = end[j + 2].min(t.len());
                            let destinations = t[j + 3..stop]
                                .iter()
                                .filter(|x| x.kind == Kind::String)
                                .filter_map(|x| value(x.text))
                                .map(str::to_owned)
                                .collect();
                            config.routes.push((key.to_string(), destinations));
                            j = stop + 1;
                        } else {
                            j += 1;
                        }
                    }
                }
            }
            this.configs.insert(parent.to_owned(), config);
        }
        this
    }
    pub fn lookup(
        &self,
        importer: &str,
        name: &str,
        paths: &HashMap<&str, usize>,
    ) -> Option<usize> {
        let (_, c) = Path::new(importer)
            .ancestors()
            .skip(1)
            .find_map(|p| self.configs.get_key_value(p.to_str()?))?;
        let mut best = None;
        for (key, tos) in &c.routes {
            if key == name {
                best = Some((usize::MAX, "", tos));
                break;
            }
            if let Some((pre, post)) = key.split_once('*')
                && let Some(middle) = name.strip_prefix(pre).and_then(|s| s.strip_suffix(post))
                && best.is_none_or(|(weight, _, _)| pre.len() > weight)
            {
                best = Some((pre.len(), middle, tos));
            }
        }
        let context = Path::new(&c.base).join("tsconfig.json");
        let file = context.to_str()?;
        let Some((_, middle, targets)) = best else {
            if c.base_url && !name.starts_with(['.', '/']) {
                return super::resolution::module_file(
                    file,
                    Language::Js,
                    &format!("./{name}"),
                    paths,
                );
            }
            return None;
        };
        for target in targets {
            let relative = format!("./{}", target.replace('*', middle));
            if let Some(id) = super::resolution::module_file(file, Language::Js, &relative, paths) {
                return Some(id);
            }
        }
        None
    }
}
