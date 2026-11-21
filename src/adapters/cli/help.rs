use std::io::{self, Write};

struct Command {
    usage: &'static str,
    purpose: &'static str,
    example: &'static str,
}

const COMMANDS: &[Command] = &[
    Command {
        usage: "search TEXT",
        purpose: "Rank passages by BM25 and return excerpts.",
        example: "search 'retry policy'",
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
        usage: "symbol NAME",
        purpose: "Find declarations by exact name.",
        example: "symbol RetryPolicy",
    },
    Command {
        usage: "definition FILE LINE COL",
        purpose: "Find what the symbol at this position refers to.",
        example: "definition src/lib.rs 42 8",
    },
    Command {
        usage: "references FILE LINE COL",
        purpose: "Find uses of the symbol at this position.",
        example: "references src/lib.rs 42 8",
    },
    Command {
        usage: "callers FILE LINE COL",
        purpose: "Find call sites and their enclosing callers.",
        example: "callers src/lib.rs 42 8",
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
    Command {
        usage: "status",
        purpose: "Show indexed HEAD, checkpoint and product generations, committed file count, size and coverage.",
        example: "status",
    },
];
const POSITIONS: &str = "FILE is relative to the current directory or absolute. LINE and COL are one-based.\nCOL counts UTF-16 code units. Search results with byte_column count UTF-8 bytes.\nCheck operation_status and relationship_status when interpreting empty results.";
pub(super) fn overview(out: &mut impl Write) -> io::Result<()> {
    writeln!(
        out,
        "Usage: cx <command> [arguments]\n\nRun a query anywhere inside a Git working tree. The first query builds the index; later queries refresh it automatically.\n"
    )?;
    for cmd in COMMANDS {
        writeln!(out, "  {:<34} {}", cmd.usage, cmd.purpose)?;
    }
    writeln!(
        out,
        "\nQueries return JSON on stdout. Check truncated on hit lists and relationship_status for semantic coverage.\nSemantic positions use one-based UTF-16 columns. Run 'cx help <command>' for an example.\n\n  cx search 'retry policy'\n  cx regex 'TODO|FIXME'\n  cx help definition"
    )
}
pub(super) fn command(name: &str, out: &mut impl Write) -> io::Result<bool> {
    let Some(cmd) = COMMANDS
        .iter()
        .find(|c| c.usage.split_whitespace().next() == Some(name))
    else {
        return Ok(false);
    };
    writeln!(
        out,
        "Usage: cx {}\n{}\n\nExample: cx {}",
        cmd.usage, cmd.purpose, cmd.example
    )?;
    match name {
        "definition" | "references" | "callers" => writeln!(out, "\n{POSITIONS}")?,
        "symbol" | "content" | "regex" => writeln!(
            out,
            "\nHits report repository-relative paths, one-based lines and one-based UTF-8 byte columns."
        )?,
        _ => {}
    }
    Ok(true)
}
