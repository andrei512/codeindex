//! Resolve per-invocation input before the watcher sees a query. Relative
//! config and pattern-file paths belong to the invoking process.
use std::{
    env, fs,
    io::{self, Read, Write},
};

fn takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--after-context"
            | "--before-context"
            | "--context"
            | "--color"
            | "--context-separator"
            | "--field-match-separator"
            | "--field-context-separator"
            | "--regexp"
            | "--glob"
            | "--type"
            | "--type-not"
            | "--type-add"
            | "--type-clear"
            | "--max-count"
            | "--path-separator"
            | "--file"
            | "--encoding"
            | "--replace"
            | "--max-columns"
            | "--sort"
            | "--sortr"
            | "--pre"
            | "--ignore-file"
            | "--max-filesize"
            | "--max-depth"
            | "--threads"
            | "--engine"
            | "--maxdepth"
            | "--iglob"
            | "--colors"
            | "--pre-glob"
            | "--dfa-size-limit"
            | "--regex-size-limit"
            | "--hyperlink-format"
    )
}

enum OptionKind {
    PatternFile,
    Value,
    Switch,
}

fn value_option(arg: &str) -> (OptionKind, usize) {
    if arg.starts_with("--") {
        let flag = arg.split('=').next().unwrap();
        return (
            if flag == "--file" {
                OptionKind::PatternFile
            } else if takes_value(flag) {
                OptionKind::Value
            } else {
                OptionKind::Switch
            },
            flag.len(),
        );
    }
    if let Some(flags) = arg.strip_prefix('-') {
        for (i, c) in flags.char_indices() {
            if "ABCegtfTmrEMjd".contains(c) {
                return (
                    if c == 'f' {
                        OptionKind::PatternFile
                    } else {
                        OptionKind::Value
                    },
                    i + 2,
                );
            }
        }
    }
    (OptionKind::Switch, arg.len())
}

fn no_config(args: &[String]) -> bool {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--no-config" {
            return true;
        }
        let (kind, end) = value_option(arg);
        if !matches!(kind, OptionKind::Switch) && arg.len() == end {
            iter.next();
        }
    }
    false
}

pub(super) fn expand(args: &mut Vec<String>, err: &mut impl Write) -> io::Result<()> {
    if let Some(path) = env::var_os("RIPGREP_CONFIG_PATH").filter(|p| !p.is_empty())
        && !no_config(args)
    {
        match fs::read_to_string(&path) {
            Ok(config) => {
                let mut defaults: Vec<_> = config
                    .lines()
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && !s.starts_with('#'))
                    .map(String::from)
                    .collect();
                defaults.append(args);
                *args = defaults;
            }
            Err(e) => writeln!(
                err,
                "cx: failed to read the file specified in RIPGREP_CONFIG_PATH: {}: {e}",
                path.to_string_lossy()
            )?,
        }
    }
    if !args.iter().any(|s| s.starts_with('-') && s.contains('f')) {
        return Ok(());
    }
    let mut expanded = Vec::with_capacity(args.len());
    let mut iter = std::mem::take(args).into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            expanded.push(arg);
            expanded.extend(iter);
            break;
        }
        let (kind, end) = value_option(&arg);
        if matches!(kind, OptionKind::PatternFile) {
            let path = if end == arg.len() {
                iter.next()
                    .ok_or_else(|| io::Error::other("missing pattern file"))?
            } else if arg.starts_with("--") {
                arg[end + 1..].to_owned()
            } else {
                arg[end..].to_owned()
            };
            if !arg.starts_with("--") && end > 2 {
                expanded.push(arg[..end - 1].to_owned());
            }
            let body = if path == "-" {
                let mut s = String::new();
                io::stdin().read_to_string(&mut s)?;
                expanded.push("--cx-pattern-stdin".into());
                s
            } else {
                fs::read_to_string(&path).map_err(|e| io::Error::other(format!("{path}: {e}")))?
            };
            expanded.push("--cx-pattern-file".into());
            for pattern in body.lines() {
                expanded.push("-e".into());
                expanded.push(pattern.into());
            }
        } else {
            let separate = matches!(kind, OptionKind::Value) && end == arg.len();
            expanded.push(arg);
            if separate && let Some(value) = iter.next() {
                expanded.push(value);
            }
        }
    }
    *args = expanded;
    Ok(())
}
