use super::{ignore::Rule, invalid, optional_bytes};
use std::{
    collections::HashMap,
    env,
    ffi::CString,
    io,
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Default)]
pub(super) struct Config {
    pub ignore_case: bool,
    pub excludes: Option<PathBuf>,
    pub sha256: bool,
    worktree_config: bool,
}
fn expanded(s: &str, home: Option<&Path>, parent: &Path) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = home
    {
        return home.join(rest);
    }
    parent.join(s)
}
struct Setting {
    section: String,
    subsection: String,
    key: String,
    value: String,
}
fn settings(data: &[u8]) -> io::Result<Vec<Setting>> {
    let mut settings = vec![];
    let (mut section, mut subsection) = (String::new(), String::new());
    let mut continued = String::new();
    for line in data.split(|&b| b == b'\n') {
        continued.push_str(&String::from_utf8_lossy(line));
        if continued.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1 {
            continued.pop();
            continue;
        }
        let joined = std::mem::take(&mut continued);
        let line = joined.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(body) = line.strip_prefix('[') {
            let end = body
                .rfind(']')
                .ok_or_else(|| invalid("invalid Git config section"))?;
            let name = body[..end].trim();
            let (tag, sub) = name.split_once(char::is_whitespace).unwrap_or((name, ""));
            section = tag.to_ascii_lowercase();
            subsection = sub.trim().trim_matches('"').to_owned();
            continue;
        }
        let (key, value) = line.split_once('=').unwrap_or((line, "true"));
        let mut val = String::new();
        let mut quoted = false;
        let mut escaped = false;
        let mut keep = 0;
        for ch in value.trim_start().chars() {
            if escaped {
                val.push(match ch {
                    'n' => '\n',
                    't' => '\t',
                    'b' => '\x08',
                    '\\' | '"' => ch,
                    _ => return Err(invalid("invalid Git config escape")),
                });
                escaped = false;
            } else {
                match ch {
                    '\\' => {
                        escaped = true;
                        continue;
                    }
                    '"' => {
                        quoted = !quoted;
                        continue;
                    }
                    ';' | '#' if !quoted => break,
                    other => val.push(other),
                }
            }
            if quoted || !ch.is_whitespace() {
                keep = val.len()
            }
        }
        if quoted || escaped {
            return Err(invalid("unterminated Git config value"));
        }
        val.truncate(keep);
        settings.push(Setting {
            section: section.clone(),
            subsection: subsection.clone(),
            key: key.trim().to_ascii_lowercase(),
            value: val,
        });
    }
    Ok(settings)
}
fn boolean(value: &str) -> io::Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" => Ok(true),
        "no" | "off" | "false" | "" => Ok(false),
        _ => value
            .parse::<i64>()
            .map(|n| n != 0)
            .map_err(|_| invalid("invalid Git boolean setting")),
    }
}
struct Reader<'a> {
    home: Option<&'a Path>,
    git: &'a Path,
    branch: Option<String>,
    remote_urls: Vec<String>,
    needs_urls: bool,
    cache: HashMap<PathBuf, Rc<Vec<Setting>>>,
}
fn matches(pattern: &str, value: &str, fold: bool) -> bool {
    // Prefixing a common component anchors basename-only patterns and preserves leading punctuation.
    let p = format!("X/{pattern}");
    let Some(rule) = Rule::parse(p.as_bytes(), 0, fold) else {
        return false;
    };
    let value = format!("X/{value}");
    let names: Option<Vec<_>> = value
        .split('/')
        .map(|n| {
            CString::new(if fold {
                n.to_ascii_lowercase()
            } else {
                n.into()
            })
            .ok()
        })
        .collect();
    names.is_some_and(|names| rule.decision(&names, false).is_some())
}
impl Reader<'_> {
    fn condition(&mut self, cond: &str, parent: &Path, second: bool) -> io::Result<bool> {
        if let Some((kind, pat)) = cond.split_once(':') {
            if kind == "gitdir" || kind == "gitdir/i" {
                let mut pattern = if pat.starts_with("~/") || pat.starts_with("./") {
                    expanded(pat, self.home, parent)
                        .to_string_lossy()
                        .replace("/./", "/")
                } else if pat.starts_with('/') {
                    pat.into()
                } else {
                    format!("**/{pat}")
                };
                if pattern.ends_with('/') {
                    pattern.push_str("**")
                }
                return Ok(matches(
                    &pattern,
                    &self.git.to_string_lossy(),
                    kind == "gitdir/i",
                ));
            }
            if kind == "onbranch" {
                if self.branch.is_none() {
                    let head = optional_bytes(&self.git.join("HEAD"))?.unwrap_or_default();
                    self.branch = Some(
                        std::str::from_utf8(&head)
                            .ok()
                            .and_then(|v| v.trim_end().strip_prefix("ref: refs/heads/"))
                            .unwrap_or("")
                            .into(),
                    );
                }
                let pattern = if pat.ends_with('/') {
                    format!("{pat}**")
                } else {
                    pat.into()
                };
                return Ok(self
                    .branch
                    .as_deref()
                    .is_some_and(|b| !b.is_empty() && matches(&pattern, b, false)));
            }
            if kind == "hasconfig"
                && let Some(p) = pat.strip_prefix("remote.*.url:")
            {
                self.needs_urls = true;
                return Ok(second && self.remote_urls.iter().any(|u| matches(p, u, false)));
            }
        }
        Ok(false)
    }
    fn apply(
        &mut self,
        path: &Path,
        c: &mut Config,
        depth: u8,
        second: bool,
        via_url: bool,
    ) -> io::Result<()> {
        if depth > 10 {
            return Err(invalid("recursive Git config include"));
        }
        let entries = if let Some(rows) = self.cache.get(path) {
            rows.clone()
        } else {
            let data = optional_bytes(path)?.unwrap_or_default();
            let rows = Rc::new(settings(&data)?);
            self.cache.insert(path.to_owned(), rows.clone());
            rows
        };
        let parent = path.parent().unwrap_or(Path::new("."));
        for entry in entries.iter() {
            let v = entry.value.as_str();
            match (entry.section.as_str(), entry.key.as_str()) {
                ("core", "excludesfile") => c.excludes = Some(expanded(v, self.home, parent)),
                ("core", "ignorecase") => c.ignore_case = boolean(v)?,
                ("extensions", "objectformat") => c.sha256 = v.eq_ignore_ascii_case("sha256"),
                ("extensions", "worktreeconfig") => c.worktree_config = boolean(v)?,
                ("include", "path") => self.apply(
                    &expanded(v, self.home, parent),
                    c,
                    depth + 1,
                    second,
                    via_url,
                )?,
                ("includeif", "path") if self.condition(&entry.subsection, parent, second)? => self
                    .apply(
                        &expanded(v, self.home, parent),
                        c,
                        depth + 1,
                        second,
                        via_url || entry.subsection.starts_with("hasconfig:"),
                    )?,
                ("remote", "url") if via_url => {
                    return Err(invalid("remote URL inside hasconfig include"));
                }
                ("remote", "url") if !second => self.remote_urls.push(v.into()),
                _ => {}
            }
        }
        Ok(())
    }
}

pub(super) fn read(common: &Path, git: &Path) -> io::Result<Config> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let xdg = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let mut paths = vec![];
    if env::var_os("GIT_CONFIG_NOSYSTEM").is_none() {
        paths.push(
            env::var_os("GIT_CONFIG_SYSTEM")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/etc/gitconfig".into()),
        )
    }
    if let Some(global) = env::var_os("GIT_CONFIG_GLOBAL") {
        paths.push(global.into())
    } else {
        if let Some(x) = &xdg {
            paths.push(x.join("git/config"))
        }
        if let Some(h) = &home {
            paths.push(h.join(".gitconfig"))
        }
    }
    paths.push(common.join("config"));
    let canonical = git.canonicalize()?;
    let mut reader = Reader {
        home: home.as_deref(),
        git: &canonical,
        branch: None,
        remote_urls: vec![],
        needs_urls: false,
        cache: HashMap::new(),
    };
    let config = || Config {
        excludes: xdg.as_ref().map(|x| x.join("git/ignore")),
        ..Default::default()
    };
    for second in [false, true] {
        let mut c = config();
        for p in &paths {
            reader.apply(p, &mut c, 0, second, false)?
        }
        if c.worktree_config {
            reader.apply(&git.join("config.worktree"), &mut c, 0, second, false)?
        }
        if second || !reader.needs_urls {
            return Ok(c);
        }
    }
    unreachable!()
}
