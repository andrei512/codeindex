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
#[derive(Clone, Copy)]
enum Field {
    Excludes,
    IgnoreCase,
    ObjectFormat,
    WorktreeConfig,
}
enum Key {
    Field(Field),
    Include(Condition),
    RemoteUrl,
}
enum Condition {
    Always,
    GitDir { pattern: String, fold: bool },
    Branch(String),
    RemoteUrl(String),
}
impl Condition {
    fn parse(s: &str) -> Option<Self> {
        let (kind, pat) = s.split_once(':')?;
        Some(match kind {
            "gitdir" | "gitdir/i" => Self::GitDir {
                pattern: pat.into(),
                fold: kind == "gitdir/i",
            },
            "onbranch" => Self::Branch(pat.into()),
            "hasconfig" => Self::RemoteUrl(pat.strip_prefix("remote.*.url:")?.into()),
            _ => return None,
        })
    }
}
impl Field {
    fn apply(&self, c: &mut Config, v: &str, home: Option<&Path>, parent: &Path) -> io::Result<()> {
        match self {
            Self::Excludes => c.excludes = Some(expanded(v, home, parent)),
            Self::IgnoreCase => c.ignore_case = boolean(v)?,
            Self::ObjectFormat => c.sha256 = v.eq_ignore_ascii_case("sha256"),
            Self::WorktreeConfig => c.worktree_config = boolean(v)?,
        }
        Ok(())
    }
}
impl Key {
    fn parse(section: &str, subsection: &str, key: &str) -> Option<Self> {
        const FIELDS: &[(&str, &str, Field)] = &[
            ("core", "excludesfile", Field::Excludes),
            ("core", "ignorecase", Field::IgnoreCase),
            ("extensions", "objectformat", Field::ObjectFormat),
            ("extensions", "worktreeconfig", Field::WorktreeConfig),
        ];
        if let Some(&(_, _, field)) = FIELDS.iter().find(|&&(s, k, _)| (s, k) == (section, key)) {
            return Some(Self::Field(field));
        }
        Some(match (section, key) {
            ("include", "path") => Self::Include(Condition::Always),
            ("includeif", "path") => Self::Include(Condition::parse(subsection)?),
            ("remote", "url") => Self::RemoteUrl,
            _ => return None,
        })
    }
}
struct Setting {
    key: Key,
    value: String,
}
fn escaped_char(ch: char) -> io::Result<char> {
    Ok(match ch {
        'n' => '\n',
        't' => '\t',
        'b' => '\x08',
        '\\' | '"' => ch,
        _ => return Err(invalid("invalid Git config escape")),
    })
}

fn unterminated_value() -> io::Error {
    invalid("unterminated Git config value")
}

fn config_value(value: &str) -> io::Result<String> {
    if !value.contains(['\\', '"', ';', '#']) {
        return Ok(value.trim().to_owned());
    }
    ValueLexer::new(value).decode()
}

struct ValueLexer<'a> {
    chars: std::str::Chars<'a>,
    quoted: bool,
}

enum ValueChar {
    Significant(char),
    PendingSpace(char),
    Quote,
    End,
}

impl ValueChar {
    fn unquoted(ch: char) -> Self {
        match ch {
            ';' | '#' => Self::End,
            space if space.is_whitespace() => Self::PendingSpace(space),
            ch => Self::Significant(ch),
        }
    }
}

impl<'a> ValueLexer<'a> {
    fn new(value: &'a str) -> Self {
        Self { chars: value.trim_start().chars(), quoted: false }
    }

    fn next(&mut self) -> io::Result<ValueChar> {
        Ok(match self.chars.next() {
            None => ValueChar::End,
            Some('\\') => return self.escape(),
            Some('"') => {
                self.quoted = !self.quoted;
                ValueChar::Quote
            }
            Some(ch) if self.quoted => ValueChar::Significant(ch),
            Some(ch) => ValueChar::unquoted(ch),
        })
    }

    fn escape(&mut self) -> io::Result<ValueChar> {
        let ch = self.chars.next().ok_or_else(unterminated_value)?;
        escaped_char(ch).map(ValueChar::Significant)
    }

    fn decode(mut self) -> io::Result<String> {
        let mut value = String::new();
        let mut keep = 0;
        loop {
            match self.next()? {
                ValueChar::Significant(ch) => {
                    value.push(ch);
                    keep = value.len();
                }
                ValueChar::PendingSpace(ch) => value.push(ch),
                ValueChar::Quote => {}
                ValueChar::End => break,
            }
        }
        if self.quoted {
            return Err(unterminated_value());
        }
        value.truncate(keep);
        Ok(value)
    }
}

#[derive(Default)]
struct Section {
    name: String,
    subsection: String,
}
impl Section {
    fn setting(&mut self, line: &str) -> io::Result<Option<Setting>> {
        if line.is_empty() || line.starts_with(['#', ';']) {
            return Ok(None);
        }
        if let Some(body) = line.strip_prefix('[') {
            let end = body
                .rfind(']')
                .ok_or_else(|| invalid("invalid Git config section"))?;
            let name = body[..end].trim();
            let (tag, sub) = name.split_once(char::is_whitespace).unwrap_or((name, ""));
            self.name = tag.to_ascii_lowercase();
            self.subsection = sub.trim().trim_matches('"').to_owned();
            return Ok(None);
        }
        let (key, value) = line.split_once('=').unwrap_or((line, "true"));
        // Even an unneeded setting must be syntactically valid.
        let value = config_value(value)?;
        Ok(Key::parse(
            &self.name,
            &self.subsection,
            &key.trim().to_ascii_lowercase(),
        )
        .map(|key| Setting { key, value }))
    }
}
fn settings(data: &[u8]) -> io::Result<Vec<Setting>> {
    let mut result = Vec::new();
    let mut section = Section::default();
    let mut logical = String::new();
    for physical in data.split(|&b| b == b'\n') {
        logical.push_str(&String::from_utf8_lossy(physical));
        if logical.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1 {
            logical.pop();
            continue;
        }
        result.extend(section.setting(logical.trim())?);
        logical.clear();
    }
    Ok(result)
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
#[derive(Clone, Copy)]
struct Include {
    depth: u8,
    second: bool,
    via_url: bool,
}
impl Include {
    fn root(second: bool) -> Self {
        Self {
            depth: 0,
            second,
            via_url: false,
        }
    }
    fn child(self, via_url: bool) -> Self {
        Self {
            depth: self.depth + 1,
            via_url: self.via_url || via_url,
            ..self
        }
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
    fn gitdir_matches(&self, pat: &str, parent: &Path, fold: bool) -> bool {
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
        matches(&pattern, &self.git.to_string_lossy(), fold)
    }

    fn branch_matches(&mut self, pat: &str) -> io::Result<bool> {
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
        Ok(self
            .branch
            .as_deref()
            .is_some_and(|b| !b.is_empty() && matches(&pattern, b, false)))
    }

    fn child_include(
        &mut self,
        condition: &Condition,
        parent: &Path,
        include: Include,
    ) -> io::Result<Option<Include>> {
        let enabled = match condition {
            Condition::Always => true,
            Condition::GitDir { pattern, fold } => self.gitdir_matches(pattern, parent, *fold),
            Condition::Branch(pattern) => self.branch_matches(pattern)?,
            Condition::RemoteUrl(pattern) => {
                self.needs_urls = true;
                include.second
                    && self
                        .remote_urls
                        .iter()
                        .any(|url| matches(pattern, url, false))
            }
        };
        Ok(enabled.then(|| include.child(matches!(condition, Condition::RemoteUrl(_)))))
    }
    fn observe_remote(&mut self, url: &str, include: Include) -> io::Result<()> {
        if include.via_url {
            return Err(invalid("remote URL inside hasconfig include"));
        }
        if !include.second {
            self.remote_urls.push(url.into());
        }
        Ok(())
    }
    fn entries(&mut self, path: &Path) -> io::Result<Rc<Vec<Setting>>> {
        Ok(if let Some(rows) = self.cache.get(path) {
            rows.clone()
        } else {
            let data = optional_bytes(path)?.unwrap_or_default();
            let rows = Rc::new(settings(&data)?);
            self.cache.insert(path.to_owned(), rows.clone());
            rows
        })
    }
    fn apply(&mut self, path: &Path, c: &mut Config, include: Include) -> io::Result<()> {
        if include.depth > 10 {
            return Err(invalid("recursive Git config include"));
        }
        let entries = self.entries(path)?;
        let parent = path.parent().unwrap_or(Path::new("."));
        for entry in entries.iter() {
            let v = entry.value.as_str();
            match &entry.key {
                Key::Field(field) => field.apply(c, v, self.home, parent)?,
                Key::Include(condition) => {
                    if let Some(child) = self.child_include(condition, parent, include)? {
                        self.apply(&expanded(v, self.home, parent), c, child)?;
                    }
                }
                Key::RemoteUrl => self.observe_remote(v, include)?,
            }
        }
        Ok(())
    }
}

fn config_paths(common: &Path, home: Option<&Path>, xdg: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![];
    if env::var_os("GIT_CONFIG_NOSYSTEM").is_none() {
        paths.push(
            env::var_os("GIT_CONFIG_SYSTEM")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/etc/gitconfig".into()),
        )
    }
    paths.extend(global_paths(home, xdg).into_iter().flatten());
    paths.push(common.join("config"));
    paths
}

fn global_paths(home: Option<&Path>, xdg: Option<&Path>) -> [Option<PathBuf>; 2] {
    match env::var_os("GIT_CONFIG_GLOBAL") {
        Some(global) => [Some(global.into()), None],
        None => [xdg.map(|dir| dir.join("git/config")), home.map(|dir| dir.join(".gitconfig"))],
    }
}

pub(super) fn read(common: &Path, git: &Path) -> io::Result<Config> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let xdg = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let paths = config_paths(common, home.as_deref(), xdg.as_deref());
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
    let initial = reader.apply_pass(&paths, git, config(), false)?;
    if reader.needs_urls {
        reader.apply_pass(&paths, git, config(), true)
    } else {
        Ok(initial)
    }
}

impl Reader<'_> {
    fn apply_pass(&mut self, paths: &[PathBuf], git: &Path, mut config: Config, second: bool) -> io::Result<Config> {
        let include = Include::root(second);
        for path in paths {
            self.apply(path, &mut config, include)?;
        }
        if config.worktree_config {
            self.apply(&git.join("config.worktree"), &mut config, include)?;
        }
        Ok(config)
    }
}
