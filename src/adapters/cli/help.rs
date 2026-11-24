use std::io::{self, Write};

struct Command {
    usage: &'static str,
    purpose: &'static str,
    example: &'static str,
}

const COMMANDS: &[Command] = &[
    Command {
        usage: "-k TEXT",
        purpose: "Rank passages by BM25 and return excerpts. Long form: --keywords TEXT.",
        example: "-k 'retry policy'",
    },
    Command {
        usage: "content TEXT",
        purpose: "Find literal text in file contents.",
        example: "content 'retry_count'",
    },
    Command {
        usage: "regex PATTERN",
        purpose: "Find regex matches in file contents, including across lines.",
        example: "regex 'TODO|FIXME'",
    },
    Command {
        usage: "path TEXT",
        purpose: "Find paths containing a literal substring.",
        example: "path migrations/",
    },
    Command {
        usage: "path-exact PATH",
        purpose: "Look up one repository-relative path.",
        example: "path-exact src/main.rs",
    },
    Command {
        usage: "path-regex PATTERN",
        purpose: "Match a regex against repository-relative paths.",
        example: r"path-regex '(^|/)test_.*\.py$'",
    },
    Command {
        usage: "glob PATTERN",
        purpose: "Match a glob against repository-relative paths.",
        example: "glob '**/*.rs'",
    },
    Command {
        usage: "--symbol NAME",
        purpose: "Find declarations by exact name.",
        example: "--symbol RetryPolicy",
    },
    Command {
        usage: "definition FILE LINE COL",
        purpose: "Find what the symbol at this position refers to.",
        example: "definition src/lib.rs 42 8",
    },
    Command {
        usage: "--reference NAME | FILE LINE COL",
        purpose: "Find uses of the symbol at this position.",
        example: "--reference src/lib.rs 42 8",
    },
    Command {
        usage: "callers NAME | FILE LINE COL",
        purpose: "Find call sites and their enclosing callers.",
        example: "callers retry_delay",
    },
    Command {
        usage: "section PATH[#ANCHOR] [--parent]",
        purpose: "Read a Markdown section or its parent.",
        example: "section 'docs/guide.md#setup' --parent",
    },
    Command {
        usage: "backlinks PATH[#ANCHOR]",
        purpose: "Find links to a Markdown document or heading.",
        example: "backlinks 'docs/guide.md#setup'",
    },
];
const POSITIONS: &str = "FILE is relative to the current directory or absolute. LINE and COL are one-based.\nCOL counts UTF-16 code units. Search results with byte_column count UTF-8 bytes.\nCheck operation_status and relationship_status when interpreting empty results.";
pub(super) fn intro(out: &mut impl Write) -> io::Result<()> {
    out.write_all(include_bytes!("intro.txt"))
}

pub(super) fn overview(out: &mut impl Write) -> io::Result<()> {
    out.write_all(b"cx PATTERN [PATH...]       Search matching source lines\n  -n line numbers, -H filenames, -i ignore case, -F literal, -A/-B/-C context\ncx -k 'topic'              Rank passages by BM25\ncx --symbol NAME           Find declarations\ncx --reference NAME        Find uses (or pass FILE LINE BYTE_COLUMN)\nOutside Git, text searches scan files and indexed queries run in memory.\nIndexed modes print path:line:source. Append --column, -C N, or --json.\nAdvanced JSON: cx --cx COMMAND ...  (callers, references, definition, content, regex, path, path-exact, path-regex, glob, symbol, section, backlinks)\nRun cx --cx help COMMAND for details.\n")
}
pub(crate) fn is_command(name: &str) -> bool {
    matches!(name, "intro" | "help") || find(name).is_some()
}

fn find(name: &str) -> Option<&'static Command> {
    let name = match name {
        "--keywords" => "-k",
        "symbol" => "--symbol",
        "references" => "--reference",
        other => other,
    };
    COMMANDS
        .iter()
        .find(|c| c.usage.split_whitespace().next() == Some(name))
}

pub(crate) fn argument_error(name: Option<&String>, error: &str) -> String {
    if let Some(cmd) = name.and_then(|name| find(name)) {
        format!(
            "{error}. Usage: cx {}. Example: cx {}",
            cmd.usage, cmd.example
        )
    } else {
        format!("{error}. Run 'cx --help' for usage.")
    }
}

pub(super) fn command(name: &str, out: &mut impl Write) -> io::Result<bool> {
    if name == "intro" {
        intro(out)?;
        return Ok(true);
    }
    let Some(cmd) = find(name) else {
        return Ok(false);
    };
    let prefix = if cmd.usage.starts_with('-') {
        ""
    } else {
        "--cx "
    };
    writeln!(
        out,
        "Usage: cx {prefix}{}\n{}\n\nExample: cx {prefix}{}",
        cmd.usage, cmd.purpose, cmd.example
    )?;
    match name {
        "definition" => writeln!(out, "\n{POSITIONS}")?,
        "--reference" => writeln!(
            out,
            "\nUse one-based UTF-8 byte columns for positional --reference. Append --json for relationship evidence."
        )?,
        "references" | "callers" => writeln!(
            out,
            "\nUse a unique declaration name or an indexed Rust inherent method such as Client::send. For ambiguous names, use cx --symbol NAME --json and select path, line and col.\n{POSITIONS}"
        )?,
        "--symbol" | "symbol" => writeln!(
            out,
            "\nFrom the repository root, use the hit's path, line and col with cx --cx callers or cx --cx references. col counts UTF-16 code units."
        )?,
        "content" | "regex" => writeln!(
            out,
            "\nHits report repository-relative paths, one-based lines and one-based UTF-8 byte columns."
        )?,
        _ => {}
    }
    Ok(true)
}
