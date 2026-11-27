use super::query::{CallerOutput, IndexedMode, Invocation, Outcome};
use crate::adapters::snapshot::Pin;
use std::{
    ffi::OsString,
    io::{self, Write},
    process::ExitCode,
};
mod help;
mod search_args;
pub(crate) fn name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::env::args_os()
            .next()
            .and_then(|s| {
                std::path::Path::new(&s)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "cx".into())
    })
}
pub fn run(
    args: impl Iterator<Item = OsString>,
    mut out: impl Write,
    mut err: impl Write,
) -> ExitCode {
    let mut args = args.peekable();
    if args
        .peek()
        .is_some_and(|arg| arg == super::watcher::INTERNAL_ARG)
    {
        args.next();
        return serve_watcher(args, &mut out);
    }
    let Ok(args) = args
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        let _ = writeln!(err, "invalid non-UTF-8 argument");
        return ExitCode::from(2);
    };
    if let Some(action) = Startup::select(&args) {
        return action.write(&mut out, &mut err);
    }
    run_search(args, &mut out, &mut err)
}

enum Startup<'a> {
    Overview,
    Intro,
    Help(&'a str),
    PcreVersion,
    Version,
}

impl<'a> Startup<'a> {
    fn select(args: &'a [String]) -> Option<Self> {
        match args {
            [flag] => Self::flag(flag),
            [ns, tail @ ..] if ns == "--cx" => Self::advanced(tail),
            _ => None,
        }
    }

    fn flag(flag: &str) -> Option<Self> {
        match flag {
            "--help" | "-h" | "--cx" => Some(Self::Overview),
            "--pcre2-version" => Some(Self::PcreVersion),
            "--version" => Some(Self::Version),
            _ => None,
        }
    }

    fn advanced(tail: &'a [String]) -> Option<Self> {
        match tail {
            [verb] if verb == "intro" => Some(Self::Intro),
            [verb, name] if verb == "help" => Some(Self::Help(name)),
            _ => None,
        }
    }

    fn write(self, out: &mut impl Write, err: &mut impl Write) -> ExitCode {
        let result = match self {
            Self::Overview => help::overview(out),
            Self::Intro => help::intro(out),
            Self::Help(command) => match help::command(command, out) {
                Ok(false) => return unknown_help(command, err),
                result => result.map(|_| ()),
            },
            Self::PcreVersion => return pcre_banner(out, err),
            Self::Version => writeln!(out, "cx {}", env!("CARGO_PKG_VERSION")),
        };
        finish_startup(result, out, err)
    }
}

fn unknown_help(command: &str, err: &mut impl Write) -> ExitCode {
    let _ = writeln!(err, "unknown command '{command}'. Run 'cx --help' to list commands.");
    ExitCode::from(2)
}

fn pcre_banner(out: &mut impl Write, err: &mut impl Write) -> ExitCode {
    let version = match super::query::pcre_version() {
        Ok(version) => version,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitCode::FAILURE;
        }
    };
    let result = writeln!(out, "{version}");
    finish_startup(result, out, err)
}

fn finish_startup(result: io::Result<()>, out: &mut impl Write, err: &mut impl Write) -> ExitCode {
    match result.and_then(|_| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(err, "{}: {error}", name());
            ExitCode::FAILURE
        }
    }
}

fn serve_watcher(mut args: impl Iterator<Item = OsString>, out: &mut impl Write) -> ExitCode {
    let (Some(root), None) = (args.next(), args.next()) else {
        return ExitCode::FAILURE;
    };
    match super::watcher::serve(std::path::Path::new(&root), out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

#[derive(Clone, Copy)]
enum SearchMode {
    Indexed(IndexedMode),
    Pattern,
    InspectPattern,
}

impl SearchMode {
    fn apply_terminal(self, args: &mut Vec<String>) {
        if matches!(self, Self::Indexed(_)) || !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            return;
        }
        let color = std::env::var_os("NO_COLOR").is_none()
            && std::env::var_os("TERM").is_none_or(|term| term != "dumb");
        let terminal = color.then(|| "--cx-terminal-color".into()).into_iter()
            .chain(std::iter::once("--cx-terminal".into()));
        args.splice(..0, terminal);
    }

    fn prepare(args: &mut Vec<String>, err: &mut impl Write) -> io::Result<Self> {
        if let Some(kind) = args.first().and_then(|arg| IndexedMode::from_flag(arg)) {
            return Ok(Self::Indexed(kind));
        }
        Ok(if search_args::expand(args, err)? {
            Self::InspectPattern
        } else {
            Self::Pattern
        })
    }
}

fn run_search(mut args: Vec<String>, out: &mut impl Write, err: &mut impl Write) -> ExitCode {
    let result =
        SearchMode::prepare(&mut args, err).and_then(|mode| execute(&mut args, mode, out, err));
    match result {
        Ok(code) if out.flush().is_ok() => code,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            let _ = writeln!(err, "{}: {e}", name());
            ExitCode::from(2)
        }
    }
}

fn execute(
    args: &mut Vec<String>,
    mode: SearchMode,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<ExitCode> {
    use super::{query::LineOptions, repository};
    let indexed = matches!(mode, SearchMode::Indexed(_));
    if args.is_empty() {
        let _ = writeln!(err, "cx: a search pattern is required");
        return Ok(ExitCode::from(2));
    }
    mode.apply_terminal(args);
    let options = if matches!(mode, SearchMode::InspectPattern) {
        let options = LineOptions::parse(args).map_err(io::Error::other)?;
        if let Some(output) = options.caller_output() {
            return caller_output(output, options, out, err);
        }
        Some(options)
    } else {
        None
    };
    // A malformed query still gets its argument diagnostic if discovery also fails.
    let location = std::env::current_dir().and_then(|cwd| {
        let root = repository::find_worktree(&cwd)?
            .filter(|path| indexed || repository::usable_marker(path));
        Ok((cwd, root))
    });
    if let Some(code) = try_watcher(options.as_ref(), &location, args, (out, err))? {
        return Ok(code);
    }
    let req = invocation(args, mode, options).map_err(io::Error::other)?;
    write_answer(req, location?, indexed, (out, err))
}

fn invocation<'a>(
    args: &'a [String],
    mode: SearchMode,
    options: Option<super::query::LineOptions<'a>>,
) -> Result<Invocation<'a>, String> {
    match (options, mode) {
        (Some(options), _) => Invocation::lines(&args[0], options),
        (_, SearchMode::Indexed(kind)) => Invocation::indexed(args, kind),
        _ => Invocation::pattern(&args[0], args),
    }
}

fn write_answer(
    req: Invocation<'_>,
    (cwd, root): (std::path::PathBuf, Option<std::path::PathBuf>),
    indexed: bool,
    (out, err): (&mut impl Write, &mut impl Write),
) -> io::Result<ExitCode> {
    let mut outcome = Outcome::default();
    let answer = match root {
        Some(root) => req.after_refresh(Pin::refresh_root(root, None).as_ref(), &cwd, &mut outcome)?,
        None => super::query::plain(req, &cwd, &mut outcome)?,
    };
    err.write_all(outcome.diagnostics.as_bytes())?;
    out.write_all(&answer)?;
    if indexed && !answer.is_empty() {
        writeln!(out)?;
    }
    Ok(ExitCode::from(outcome.code))
}

fn try_watcher(
    options: Option<&super::query::LineOptions<'_>>,
    location: &io::Result<(std::path::PathBuf, Option<std::path::PathBuf>)>,
    args: &[String],
    (out, err): (&mut impl Write, &mut impl Write),
) -> io::Result<Option<ExitCode>> {
    let Ok((cwd, Some(root))) = location else { return Ok(None) };
    if options.is_some_and(super::query::LineOptions::requires_caller) {
        return Ok(None);
    }
    super::watcher::query(super::watcher::CallerQuery { root, cwd, args }, (out, err))
}

fn caller_output(output: CallerOutput, options: super::query::LineOptions, out: &mut impl Write, err: &mut impl Write) -> io::Result<ExitCode> {
    let lines = options.finish().map_err(io::Error::other)?;
    match output {
        CallerOutput::Types => {
            out.write_all(lines.type_listing().expect("type listing was requested").as_bytes())?;
            Ok(ExitCode::SUCCESS)
        }
        CallerOutput::Empty => Ok(ExitCode::FAILURE),
        CallerOutput::Stdin => {
            use std::io::Read;
            err.write_all(lines.stdin_warnings()?.as_bytes())?;
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input)?;
            let (text, hit) = lines.stream(&input);
            out.write_all(&text)?;
            Ok(ExitCode::from(u8::from(!hit)))
        }
    }
}
