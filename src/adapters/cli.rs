use super::query::{Invocation, Outcome, evaluate};
use crate::adapters::snapshot::Pin;
use std::{
    ffi::OsString,
    io::{self, Write},
    process::ExitCode,
};
mod help;
pub(crate) use help::{argument_error, is_command};
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
        return match (args.next(), args.next()) {
            (Some(root), None)
                if super::watcher::serve(std::path::Path::new(&root), &mut out).is_ok() =>
            {
                ExitCode::SUCCESS
            }
            _ => ExitCode::FAILURE,
        };
    }
    let Ok(mut args) = args
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        let _ = writeln!(err, "invalid non-UTF-8 argument");
        return ExitCode::from(2);
    };
    let result = if matches!(args.as_slice(), [flag] if flag == "--help" || flag == "-h" || flag == "--cx")
    {
        help::overview(&mut out)
    } else if args.as_slice() == ["--cx", "intro"] {
        help::intro(&mut out)
    } else if let [ns, verb, name] = args.as_slice()
        && ns == "--cx"
        && verb == "help"
    {
        match help::command(name, &mut out) {
            Ok(true) => Ok(()),
            Ok(false) => {
                let _ = writeln!(
                    err,
                    "unknown command '{name}'. Run 'cx --help' to list commands."
                );
                return ExitCode::from(2);
            }
            Err(e) => Err(e),
        }
    } else if args.as_slice() == ["--version"] {
        writeln!(out, "cx {}", env!("CARGO_PKG_VERSION"))
    } else {
        if args.is_empty() {
            let _ = writeln!(err, "cx: a search pattern is required");
            return ExitCode::from(2);
        }
        if std::io::IsTerminal::is_terminal(&std::io::stdout())
            && !matches!(
                args[0].as_str(),
                "--score" | "--cx" | "-k" | "--keywords" | "--symbol" | "--reference" | "--refs"
            )
        {
            args.insert(0, "--cx-terminal".into());
            if std::env::var_os("NO_COLOR").is_none()
                && std::env::var_os("TERM").is_none_or(|t| t != "dumb")
            {
                args.insert(0, "--cx-terminal-color".into());
            }
        }
        let req = match Invocation::parse(&args) {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(err, "cx: {e}");
                return ExitCode::from(2);
            }
        };
        if let super::query::Request::Lines(lines) = &req.request {
            if let Some(list) = lines.type_listing() {
                return if out.write_all(list.as_bytes()).and_then(|_| out.flush()).is_ok() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(2)
                };
            }
            use std::os::unix::fs::FileTypeExt;
            let piped = std::fs::metadata("/dev/stdin")
                .is_ok_and(|m| m.is_file() || m.file_type().is_fifo() || m.file_type().is_socket());
            if lines.explicit_stdin() || piped && lines.stdin_only() {
                use std::io::Read;
                let mut input = Vec::new();
                if let Err(e) = std::io::stdin().read_to_end(&mut input) {
                    let _ = writeln!(err, "cx: {e}");
                    return ExitCode::from(2);
                }
                let (text, hit) = lines.stream(&input);
                if out
                    .write_all(&text)
                    .and_then(|_| out.flush())
                    .is_err()
                {
                    return ExitCode::from(2);
                }
                return ExitCode::from(u8::from(!hit));
            }
        }
        return match (|| -> io::Result<ExitCode> {
            let cwd = std::env::current_dir()?;
            let is_lines = matches!(req.request, super::query::Request::Lines(_));
            let mut result = Outcome::default();
            let answer = if let Some(root) = super::repository::find_worktree(&cwd)? {
                if let Some(code) = super::watcher::query(&root, &cwd, &args, &mut out, &mut err)? {
                    return Ok(code);
                }
                evaluate(&Pin::refresh_root(root, None)?, req, &cwd, &mut result)?
            } else {
                super::query::plain(req, &cwd, &mut result)?
            };
            err.write_all(result.diagnostics.as_bytes())?;
            if answer.is_empty() {
                return Ok(ExitCode::from(result.code));
            }
            out.write_all(&answer)?;
            if !is_lines {
                writeln!(out)?;
            }
            Ok(ExitCode::from(result.code))
        })() {
            Ok(code) if out.flush().is_ok() => code,
            Ok(_) => ExitCode::FAILURE,
            Err(e) => {
                let _ = writeln!(err, "cx: {e}");
                ExitCode::from(2)
            }
        };
    };
    match result.and_then(|_| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(err, "cx: {e}");
            ExitCode::FAILURE
        }
    }
}
