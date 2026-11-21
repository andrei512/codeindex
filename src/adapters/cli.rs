use super::query::{Request, evaluate};
use crate::adapters::snapshot::Pin;
use std::{
    ffi::OsString,
    io::{self, Write},
    process::ExitCode,
};
mod help;
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
    let Ok(args) = args
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        let _ = writeln!(err, "invalid non-UTF-8 argument");
        return ExitCode::from(2);
    };
    let result = if args.is_empty()
        || (args.len() == 1 && matches!(args[0].as_str(), "help" | "--help" | "-h"))
    {
        help::overview(&mut out)
    } else if let [verb, name] = args.as_slice()
        && verb == "help"
    {
        match help::command(name, &mut out) {
            Ok(true) => Ok(()),
            Ok(false) => {
                let _ = writeln!(
                    err,
                    "unknown command '{name}'. Run 'cx help' to list commands."
                );
                return ExitCode::from(2);
            }
            Err(e) => Err(e),
        }
    } else if args.as_slice() == ["--version"] {
        writeln!(out, "cx {}", env!("CARGO_PKG_VERSION"))
    } else {
        if let Ok(cwd) = std::env::current_dir() {
            match super::watcher::query(&cwd, &args, &mut out, &mut err) {
                Ok(Some(code)) => return code,
                Ok(None) => {}
                Err(e) => {
                    let _ = writeln!(err, "cx: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        let req = match Request::parse(&args) {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(err, "{e}. Run 'cx help' for usage.");
                return ExitCode::from(2);
            }
        };
        (|| -> io::Result<()> {
            let cwd = std::env::current_dir()?;
            let pin = Pin::refresh(&cwd)?;
            let answer = evaluate(&pin, req, &args[0], &cwd)?;
            writeln!(out, "{answer}")
        })()
    };
    match result.and_then(|_| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(err, "cx: {e}");
            ExitCode::FAILURE
        }
    }
}
