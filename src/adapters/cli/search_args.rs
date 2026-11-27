//! Parse search syntax at the process boundary. The watcher receives canonical
//! options, expanded patterns and a separate positional tail.
use clap::{
    Arg, ArgAction, Command,
    error::{ContextKind, ErrorKind},
};
use std::{
    env, fs,
    io::{self, Read, Write},
};

use crate::adapters::query::ValueOption;

fn grammar() -> Command {
    let mut cmd = Command::new("cx")
        .no_binary_name(true)
        .disable_help_flag(true);
    let values = ValueOption::ALL
        .iter()
        .map(|option| (option.name, option.short, true))
        .chain(std::iter::once(("file", 'f', true)));
    let switches =
        crate::adapters::query::switch_spellings().map(|(name, short)| (name, short, false));
    for (name, short, takes_value) in values.chain(switches) {
        cmd = cmd.arg(option(name, short, takes_value));
    }
    cmd.arg(Arg::new("paths").num_args(0..).action(ArgAction::Append))
}

fn option(name: &'static str, short: char, takes_value: bool) -> Arg {
    let base = Arg::new(name).long(name).action(ArgAction::Append);
    let arg = if short == '\0' { base } else { base.short(short) };
    let arg = if name == "max-depth" { arg.alias("maxdepth") } else { arg };
    if takes_value {
        arg.num_args(1).allow_hyphen_values(true)
    } else {
        arg.num_args(0).default_missing_value("1")
    }
}

fn diagnostic(error: clap::Error, args: &[String]) -> io::Error {
    let arg = error
        .get(ContextKind::InvalidArg)
        .map(ToString::to_string)
        .unwrap_or_default();
    let message = match (error.kind(), arg.as_str()) {
        (ErrorKind::InvalidValue, _)
            if error
                .get(ContextKind::InvalidValue)
                .is_some_and(|v| v.to_string().is_empty()) =>
        {
            missing_value(&error, args, &arg)
        }
        (ErrorKind::TooManyValues, _) => {
            let value = error
                .get(ContextKind::InvalidValue)
                .map(ToString::to_string)
                .unwrap_or_default();
            format!("invalid CLI arguments: unexpected argument for option '{arg}': {value:?}")
        }
        (ErrorKind::UnknownArgument, "--include") => {
            "unrecognized flag --include\n\nsimilar flags that are available: --include-zero"
                .to_owned()
        }
        (ErrorKind::UnknownArgument, _) => format!("unrecognized flag {arg}"),
        _ => error.to_string().trim().to_owned(),
    };
    io::Error::other(message)
}

fn missing_value(error: &clap::Error, args: &[String], arg: &str) -> String {
    let canonical = arg.split_whitespace().next().unwrap_or("");
    if canonical == "--file" {
        return "missing pattern file".to_string();
    }
    let spelling = args.last().map(String::as_str).unwrap_or(canonical);
    ValueOption::parse(canonical).map_or_else(
        || error.to_string().trim().to_owned(),
        |option| option.missing(spelling),
    )
}

fn append_patterns(args: &mut Vec<String>, path: &str) -> io::Result<()> {
    let body = if path == "-" {
        let mut source = String::new();
        io::stdin().read_to_string(&mut source)?;
        args.push("--cx-pattern-stdin".into());
        source
    } else {
        fs::read_to_string(path).map_err(|e| io::Error::other(format!("{path}: {e}")))?
    };
    args.push("--cx-pattern-file".into());
    for pattern in body.lines() {
        args.push("--regexp".into());
        args.push(pattern.into());
    }
    Ok(())
}

fn parse(args: &[String]) -> io::Result<clap::ArgMatches> {
    grammar()
        .try_get_matches_from(args)
        .map_err(|e| diagnostic(e, args))
}

struct CanonicalOption<'a> {
    key: &'a str,
    value: Option<&'a String>,
}

impl CanonicalOption<'_> {
    fn write(self, args: &mut Vec<String>) -> io::Result<()> {
        if self.key == "file" {
            return append_patterns(args, self.value.unwrap());
        }
        args.push(format!("--{}", self.key));
        args.extend(self.value.cloned());
        Ok(())
    }
}

fn ordered_options(parsed: &clap::ArgMatches) -> Vec<(usize, CanonicalOption<'_>)> {
    let mut events: Vec<_> = parsed.ids().filter(|id| id.as_str() != "paths").flat_map(|id| {
        let key = id.as_str();
        let takes_value = key == "file" || ValueOption::named(key).is_some();
        let indices = parsed.indices_of(key).unwrap();
        indices.zip(parsed.get_many::<String>(key).unwrap()).map(move |(index, value)| {
            (index, CanonicalOption { key, value: takes_value.then_some(value) })
        })
    }).collect();
    events.sort_unstable_by_key(|event| event.0);
    events
}

fn config_options(args: &mut Vec<String>, err: &mut impl Write) -> io::Result<Option<clap::ArgMatches>> {
    let Some(path) = env::var_os("RIPGREP_CONFIG_PATH").filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let parsed = args.iter().any(|arg| arg == "--no-config").then(|| parse(args)).transpose()?;
    if parsed.as_ref().is_some_and(|options| options.contains_id("no-config")) {
        return Ok(parsed);
    }
    match fs::read_to_string(&path) {
        Ok(config) => {
            let defaults = config.lines().map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(String::from);
            args.splice(..0, defaults);
            Ok(None)
        }
        Err(error) => {
            writeln!(err, "cx: failed to read the file specified in RIPGREP_CONFIG_PATH: {}: {error}", path.to_string_lossy())?;
            Ok(parsed)
        }
    }
}

fn inspect_locally(parsed: &clap::ArgMatches) -> bool {
    // Ordinary file searches are decoded by the worker. Stdin and commands
    // that launch a process or need no index are resolved in the caller.
    let paths = parsed.get_many::<String>("paths");
    let may_read_stdin = paths.is_none_or(|paths| paths.len() <= 1 || paths.into_iter().any(|p| p == "-"));
    may_read_stdin || ["max-count", "type-list", "search-zip", "pre"]
        .iter().any(|id| parsed.contains_id(id))
}

pub(super) fn expand(args: &mut Vec<String>, err: &mut impl Write) -> io::Result<bool> {
    let mut parsed = config_options(args, err)?.map_or_else(|| parse(args), Ok)?;
    let inspect = inspect_locally(&parsed);
    let events = ordered_options(&parsed);
    args.clear();
    for (_, event) in events {
        event.write(args)?;
    }
    if let Some(paths) = parsed.remove_many::<String>("paths") {
        args.push("--".into());
        args.extend(paths);
    }
    Ok(inspect)
}
