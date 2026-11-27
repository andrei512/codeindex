mod display;
mod json;
use display::Display;
mod lines;
pub(crate) use lines::pcre::version as pcre_version;
pub(crate) use lines::{CallerOutput, Options as LineOptions, ValueOption, switch_spellings};
mod navigation;
mod relationships;
pub(super) use relationships::Operation as RelationshipOperation;
mod score;
use crate::{
    adapters::snapshot::Pin,
    adapters::snapshot::manifest::*,
    domain::{
        content_lookup::Cursor,
        markdown::is_markdown,
        pattern::{Glob, Regex, Scratch},
        ranked::RankedIndex,
    },
};
use json::{object, quote};
use navigation::{DocumentQuery, Markdown};
use std::{collections::BTreeSet, io, path::Path};

#[derive(Clone, Copy)]
pub struct ScoreOptions {
    json: bool,
    test_rules: bool,
    project_policy: bool,
}

pub enum Request<'a> {
    Lines(lines::Lines<'a>),
    Roles(Option<String>, bool),
    Score(Option<String>, ScoreOptions),
    Page(PageQuery<'a>),
    Document(DocumentQuery<'a>),
    Relationship(relationships::Query<'a>),
}
pub enum PageQuery<'a> {
    Search(&'a str),
    Symbol(&'a str),
    Content(&'a str),
    Regex(Regex),
    Paths(PathQuery<'a>),
}
pub enum PathQuery<'a> {
    Contains(&'a str),
    Regex(Regex),
    Exact(&'a str),
    Glob(Glob),
}

impl PathQuery<'_> {
    fn collect(self, pin: &Pin, page: &mut page::Page) -> io::Result<()> {
        match self {
            Self::Contains(q) => {
                page.paths(pin, candidates(pin, PATHGRAM, q)?, |path| path.contains(q));
            }
            Self::Regex(re) => {
                let mut scratch = Scratch::default();
                page.paths(
                    pin,
                    candidates(pin, PATHGRAM, re.required_literal())?,
                    |path| re.matches_with(path, &mut scratch).next().is_some(),
                );
            }
            Self::Exact(q) => {
                if let Ok(i) = pin.files.binary_search_by(|f| f.path().cmp(q)) {
                    page.path(pin.files[i].path());
                }
            }
            Self::Glob(g) => {
                let mut scratch = Scratch::default();
                page.paths(pin, 0..pin.files.len(), |path| {
                    g.matches_with(path, &mut scratch)
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    Rank,
    Page(PageCommand),
    Relationship(relationships::Operation),
    Section,
    Backlinks,
    Intro,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageCommand {
    Content,
    Regex,
    Path,
    PathExact,
    PathRegex,
    Glob,
    Symbol,
}

impl Command {
    pub(crate) fn advanced(name: &str) -> Option<Self> {
        use relationships::Operation;
        use PageCommand::*;
        const SPELLINGS: &[(&str, Command)] = &[
            ("rank", Command::Rank),
            ("-k", Command::Rank),
            ("--keywords", Command::Rank),
            ("content", Command::Page(Content)),
            ("regex", Command::Page(Regex)),
            ("path", Command::Page(Path)),
            ("path-exact", Command::Page(PathExact)),
            ("path-regex", Command::Page(PathRegex)),
            ("glob", Command::Page(Glob)),
            ("symbol", Command::Page(Symbol)),
            ("--symbol", Command::Page(Symbol)),
            ("definition", Command::Relationship(Operation::Definition)),
            ("references", Command::Relationship(Operation::References)),
            ("--reference", Command::Relationship(Operation::References)),
            ("callers", Command::Relationship(Operation::Callers)),
            ("section", Command::Section),
            ("backlinks", Command::Backlinks),
            ("intro", Command::Intro),
            ("help", Command::Help),
        ];
        SPELLINGS.iter().find(|&&(spelling, _)| spelling == name).map(|&(_, command)| command)
    }
}

impl<'a> Request<'a> {
    fn parse_public(command: Command, args: &'a [String]) -> Result<(Self, Display), String> {
        let (request, flags) = match command {
            Command::Relationship(operation) => {
                let (query, flags) = relationships::Query::parse_public(operation, args)?;
                (Self::Relationship(query), flags)
            }
            _ => {
                let (operands, flags) = args.split_at_checked(1).ok_or("missing indexed query operand")?;
                (Self::parse_command(command, operands)?, flags)
            }
        };
        Ok((request, Display::parse(flags)?))
    }

    fn parse_command(command: Command, operands: &'a [String]) -> Result<Self, String> {
        match command {
            Command::Relationship(operation) => relationships::Query::parse(operation, operands).map(Self::Relationship),
            Command::Section => DocumentQuery::section(operands).map(Self::Document),
            _ => Self::parse_single_operand(command, operands),
        }
    }

    fn parse_single_operand(command: Command, operands: &'a [String]) -> Result<Self, String> {
        match (command, operands) {
            (Command::Rank, [q]) if !q.trim().is_empty() => Ok(Self::Page(PageQuery::Search(q))),
            (Command::Backlinks, [handle]) => Ok(Self::Document(DocumentQuery::Backlinks(handle))),
            (Command::Page(page), [q]) if !q.is_empty() => PageQuery::parse(page, q).map(Self::Page),
            _ => Err("invalid query arguments".into()),
        }
    }
}

impl<'a> DocumentQuery<'a> {
    fn section(operands: &'a [String]) -> Result<Self, String> {
        match operands {
            [handle] => Ok(Self::Section { handle, parent: false }),
            [handle, flag] if flag == "--parent" => Ok(Self::Section { handle, parent: true }),
            _ => Err("invalid query arguments".into()),
        }
    }
}

impl<'a> PageQuery<'a> {
    fn parse(command: PageCommand, query: &'a str) -> Result<Self, String> {
        Ok(match command {
            PageCommand::Content => Self::Content(query),
            PageCommand::Regex => Self::Regex(Regex::new(query)?),
            PageCommand::Path => Self::Paths(PathQuery::Contains(query)),
            PageCommand::PathRegex => Self::Paths(PathQuery::Regex(Regex::new(query)?)),
            PageCommand::PathExact => Self::Paths(PathQuery::Exact(query)),
            PageCommand::Glob => Self::Paths(PathQuery::Glob(Glob::new(query)?)),
            PageCommand::Symbol => Self::Symbol(query),
        })
    }
}

/// A full scan needs only the file count. Indexed searches own their posting list.
enum Candidates {
    All(usize),
    Postings(Vec<usize>),
}

impl Candidates {
    fn len(&self) -> usize {
        match self {
            Self::All(count) => *count,
            Self::Postings(ids) => ids.len(),
        }
    }
}

impl IntoIterator for Candidates {
    type Item = usize;
    type IntoIter = std::iter::Chain<std::ops::Range<usize>, std::vec::IntoIter<usize>>;

    fn into_iter(self) -> Self::IntoIter {
        let (count, postings) = match self {
            Self::All(count) => (count, Vec::new()),
            Self::Postings(ids) => (0, ids),
        };
        (0..count).chain(postings)
    }
}

fn candidates(pin: &Pin, product: usize, q: &str) -> io::Result<Candidates> {
    let count = pin.files.len();
    if pin.is_memory() {
        return Ok(Candidates::All(count));
    }
    let keys = query_trigrams(q);
    if keys.is_empty() {
        return Ok(Candidates::All(count));
    }
    Cursor::open(pin.reader(product)?)?
        .candidates(keys, count)
        .map(Candidates::Postings)
}

fn query_trigrams(text: &str) -> BTreeSet<[char; 3]> {
    let mut chars = text.chars().take(18);
    let mut keys = BTreeSet::new();
    if let (Some(mut a), Some(mut b)) = (chars.next(), chars.next()) {
        for c in chars {
            keys.insert([a, b, c]);
            (a, b) = (b, c);
        }
    }
    keys
}

fn breadcrumbs(
    pin: &Pin,
    md: &mut Option<Markdown>,
    file: usize,
    offset: usize,
) -> io::Result<Option<String>> {
    if !is_markdown(pin.files[file].path()) {
        return Ok(None);
    }
    if md.is_none() {
        *md = Some(Markdown::read(pin)?);
    }
    md.as_mut().unwrap().hit_crumbs(pin, file, offset).map(Some)
}
#[derive(Default)]
pub struct Outcome {
    pub code: u8,
    pub diagnostics: String,
}
pub struct Invocation<'a> {
    pub request: Request<'a>,
    pub mode: &'a str,
    display: Display,
}
fn score_request(args: &[String]) -> Result<Request<'static>, String> {
    use clap::{Arg, ArgAction, Command};
    let switch = |name: &'static str| Arg::new(name).long(name).action(ArgAction::SetTrue);
    let mut flags = Command::new("score")
        .no_binary_name(true)
        .disable_help_flag(true)
        .arg(Arg::new("path"))
        .arg(switch("json"))
        .arg(switch("roles").conflicts_with_all(["ignore-test-rules", "ignore-score-policy"]))
        .arg(switch("ignore-test-rules"))
        .arg(switch("ignore-score-policy"))
        .try_get_matches_from(args)
        .map_err(|_| {
            "usage: cx --score [FILE|DIR] [--roles | --ignore-score-policy] [--json]".to_string()
        })?;
    let path = flags.remove_one("path");
    let json = flags.get_flag("json");
    Ok(if flags.get_flag("roles") {
        Request::Roles(path, json)
    } else {
        Request::Score(
            path,
            ScoreOptions {
                json,
                test_rules: !flags.get_flag("ignore-test-rules"),
                project_policy: !flags.get_flag("ignore-score-policy"),
            },
        )
    })
}

/// The first argument selects the index and query grammar before config expansion.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexedMode {
    Score,
    Advanced,
    Public(Command),
}

impl IndexedMode {
    pub fn from_flag(flag: &str) -> Option<Self> {
        Some(match flag {
            "--score" => Self::Score,
            "--cx" => Self::Advanced,
            "--reference" | "--refs" => Self::Public(Command::Relationship(relationships::Operation::References)),
            "-k" | "--keywords" => Self::Public(Command::Rank),
            "--symbol" => Self::Public(Command::Page(PageCommand::Symbol)),
            _ => return None,
        })
    }
}

impl<'a> Invocation<'a> {
    pub fn parse(args: &'a [String]) -> Result<Self, String> {
        let mode = args.first().ok_or("missing pattern")?.as_str();
        match IndexedMode::from_flag(mode) {
            Some(kind) => Self::indexed(args, kind),
            None => Self::pattern(mode, args),
        }
    }

    /// Canonical pattern arguments already passed mode selection at the boundary.
    pub(crate) fn pattern(mode: &'a str, args: &'a [String]) -> Result<Self, String> {
        Self::lines(mode, LineOptions::parse(args)?)
    }

    pub(crate) fn indexed(args: &'a [String], kind: IndexedMode) -> Result<Self, String> {
        let mode = args[0].as_str();
        let (request, mode, display) = match kind {
            IndexedMode::Score => (
                score_request(&args[1..])?,
                mode,
                Display::default(),
            ),
            IndexedMode::Advanced => {
                let tail = &args[1..];
                let name = tail.first().ok_or("missing indexed command")?.as_str();
                let command = Command::advanced(name).ok_or("unknown indexed command")?;
                (Request::parse_command(command, &tail[1..])?, name, Display::default())
            }
            IndexedMode::Public(command) => {
                let (request, display) = Request::parse_public(command, &args[1..])?;
                (request, mode, display)
            }
        };
        Ok(Self { request, mode, display })
    }

    pub(crate) fn lines(mode: &'a str, options: LineOptions<'a>) -> Result<Self, String> {
        Ok(Self {
            request: Request::Lines(options.finish()?),
            mode,
            display: Display::default(),
        })
    }
}
pub(crate) fn plain(
    invocation: Invocation<'_>,
    cwd: &Path,
    outcome: &mut Outcome,
) -> io::Result<Vec<u8>> {
    invocation.execute(None, cwd, outcome)
}

pub fn evaluate(
    pin: &Pin,
    invocation: Invocation<'_>,
    cwd: &Path,
    outcome: &mut Outcome,
) -> io::Result<Vec<u8>> {
    invocation.execute(Some(pin), cwd, outcome)
}

mod page;
impl Invocation<'_> {
    /// A line query can read its operands directly when snapshot refresh fails.
    pub(crate) fn after_refresh(
        self,
        pin: Result<&Pin, &io::Error>,
        cwd: &Path,
        outcome: &mut Outcome,
    ) -> io::Result<Vec<u8>> {
        match pin {
            Ok(pin) => evaluate(pin, self, cwd, outcome),
            Err(_) if matches!(self.request, Request::Lines(_)) => self.execute(None, cwd, outcome),
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        }
    }

    fn execute(self, pin: Option<&Pin>, cwd: &Path, outcome: &mut Outcome) -> io::Result<Vec<u8>> {
        use crate::application::index::build::MemoryQuery;
        let Self {
            request,
            mode,
            display,
        } = self;
        let mut source = QuerySource {
            cwd,
            current: pin,
            temporary: None,
        };
        let result = match request {
            Request::Lines(lines) => return lines.walk_search(cwd, outcome, pin),
            Request::Roles(path, json) => source.roles(path.as_deref(), json, outcome),
            Request::Score(path, options) => source.score(path.as_deref(), options, outcome),
            Request::Relationship(query) => query.run(
                source.index(|| MemoryQuery::Relationships)?,
                cwd,
                &display,
                outcome,
            ),
            Request::Document(query) => query.run(pin, cwd),
            Request::Page(query) => query.run(&mut source, mode, &display, outcome),
        };
        result.map(String::into_bytes)
    }
}

struct QuerySource<'a> {
    cwd: &'a Path,
    current: Option<&'a Pin>,
    temporary: Option<Pin>,
}

impl QuerySource<'_> {
    fn roles(&mut self, path: Option<&str>, json: bool, outcome: &mut Outcome) -> io::Result<String> {
        use crate::application::index::build::MemoryQuery;
        let request = score::RoleRequest { cwd: self.cwd, path, json };
        score::roles(self.index(|| MemoryQuery::Source)?, request, outcome)
    }

    fn score(&self, path: Option<&str>, options: ScoreOptions, outcome: &mut Outcome) -> io::Result<String> {
        match self.current {
            Some(pin) => score::query(pin, self.cwd, score::Request { path, options }, outcome),
            None => score::plain(self.cwd, path, options, outcome),
        }
    }

    fn index<'a>(
        &mut self,
        product: impl FnOnce() -> crate::application::index::build::MemoryQuery<'a>,
    ) -> io::Result<&Pin> {
        if let Some(pin) = self.current {
            return Ok(pin);
        }
        Ok(self.temporary.insert(Pin::memory(self.cwd, product())?))
    }
}
