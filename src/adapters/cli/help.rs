use super::super::query::{Command, PageCommand, RelationshipOperation as Operation};
use std::io::{self, Write};

struct Help {
    command: Command,
    usage: &'static str,
    purpose: &'static str,
    example: &'static str,
}

const COMMANDS: &[Help] = &[
    Help {
        command: Command::Rank,
        usage: "-k TEXT",
        purpose: "Rank passages by BM25 and return excerpts. Long form: --keywords TEXT.",
        example: "-k 'retry policy'",
    },
    Help {
        command: Command::Page(PageCommand::Content),
        usage: "content TEXT",
        purpose: "Find literal text in file contents.",
        example: "content 'retry_count'",
    },
    Help {
        command: Command::Page(PageCommand::Regex),
        usage: "regex PATTERN",
        purpose: "Find regex matches in file contents, including across lines.",
        example: "regex 'TODO|FIXME'",
    },
    Help {
        command: Command::Page(PageCommand::Path),
        usage: "path TEXT",
        purpose: "Find paths containing a literal substring.",
        example: "path migrations/",
    },
    Help {
        command: Command::Page(PageCommand::PathExact),
        usage: "path-exact PATH",
        purpose: "Look up one repository-relative path.",
        example: "path-exact src/main.rs",
    },
    Help {
        command: Command::Page(PageCommand::PathRegex),
        usage: "path-regex PATTERN",
        purpose: "Match a regex against repository-relative paths.",
        example: r"path-regex '(^|/)test_.*\.py$'",
    },
    Help {
        command: Command::Page(PageCommand::Glob),
        usage: "glob PATTERN",
        purpose: "Match a glob against repository-relative paths.",
        example: "glob '**/*.rs'",
    },
    Help {
        command: Command::Page(PageCommand::Symbol),
        usage: "--symbol NAME",
        purpose: "Find declarations by exact name.",
        example: "--symbol RetryPolicy",
    },
    Help {
        command: Command::Relationship(Operation::Definition),
        usage: "definition FILE LINE COL",
        purpose: "Find what the symbol at this position refers to.",
        example: "definition src/lib.rs 42 8",
    },
    Help {
        command: Command::Relationship(Operation::References),
        usage: "--reference NAME | FILE LINE COL",
        purpose: "Find uses of the symbol at this position.",
        example: "--reference src/lib.rs 42 8",
    },
    Help {
        command: Command::Relationship(Operation::Callers),
        usage: "callers NAME | FILE LINE COL",
        purpose: "Find call sites and their enclosing callers.",
        example: "callers retry_delay",
    },
    Help {
        command: Command::Section,
        usage: "section PATH[#ANCHOR] [--parent]",
        purpose: "Read a Markdown section or its parent.",
        example: "section 'docs/guide.md#setup' --parent",
    },
    Help {
        command: Command::Backlinks,
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
    out.write_all(b"cx PATTERN [PATH...]       Search matching source lines\n  -n line numbers, -H filenames, -i ignore case, -F literal, -A/-B/-C context\n  -g GLOB, -t TYPE, -l matching paths, -a binary as text\ncx -k 'topic'              Rank passages by BM25\ncx --symbol NAME           Find declarations\ncx --score [FILE|DIR]      Explain maintainability (1-10); --roles identifies tests and helpers; --ignore-score-policy uses original thresholds and coverage; --json for structured output\ncx --reference NAME        Find uses (or pass FILE LINE BYTE_COLUMN)\nOutside Git, text searches scan files and indexed queries run in memory.\nIndexed modes print path:line:source. Append --column, -C N, or --json.\nAdvanced JSON: cx --cx COMMAND ...  (callers, references, definition, content, regex, path, path-exact, path-regex, glob, symbol, section, backlinks)\nRun cx --cx help COMMAND for details.\n")
}
fn find(name: &str) -> Option<&'static Help> {
    if name == "rank" {
        return None;
    }
    let command = Command::advanced(name)?;
    COMMANDS.iter().find(|entry| entry.command == command)
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
    write_notes(name, out)?;
    Ok(true)
}

fn write_notes(name: &str, out: &mut impl Write) -> io::Result<()> {
    match name {
        "definition" => writeln!(out, "\n{POSITIONS}"),
        "--reference" => writeln!(
            out,
            "\nUse one-based UTF-8 byte columns for positional --reference. Append --json for relationship evidence."
        ),
        "references" | "callers" => writeln!(
            out,
            "\nUse a unique declaration name or an indexed Rust inherent method such as Client::send. For ambiguous names, use cx --symbol NAME --json and select path, line and col.\n{POSITIONS}"
        ),
        "--symbol" | "symbol" => writeln!(
            out,
            "\nFrom the repository root, use the hit's path, line and col with cx --cx callers or cx --cx references. col counts UTF-16 code units."
        ),
        "content" | "regex" => writeln!(
            out,
            "\nHits report repository-relative paths, one-based lines and one-based UTF-8 byte columns."
        ),
        _ => Ok(()),
    }
}
