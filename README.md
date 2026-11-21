# Codeindex

Codeindex gives coding agents a local, searchable map of a Git working tree. It installs one command, `cx`. An agent can search for a concept in the docs, jump to a declaration, and follow a call back to the code that uses it. Each answer is JSON with repository-relative paths and source locations the agent can open next.

Before changing a retry policy, an agent needs to know where it is documented, which function implements the delay, and who calls that function. Those are three different questions about the same checkout. Codeindex indexes source text, paths, code relationships, and Markdown together so an agent can follow that trail across files.

Run a query anywhere inside the working tree. The first query builds the local index under `~/.codeindex`. Later queries refresh it against your working files, including uncommitted edits and eligible untracked files. There is no separate build command or server to configure.

## Install

```sh
cargo install --locked codeindex
```

To install from a checkout, run `cargo install --locked --path .`. The CLI targets macOS and Linux. On macOS, a watcher keeps the index warm for five minutes after the last request.

## From a question to the code

These examples use a small Rust client with a retry guide. `cx` writes JSON directly. The optional `jq` filters show the fields an agent would use for its next step.

### Where is the retry policy documented?

`search` ranks passages with BM25. A Markdown hit includes the heading trail and a handle for the matching section.

```sh
cx search 'retry policy' | jq '.hits[0] | {path, line, excerpt, section: .breadcrumbs[-1].handle}'
```

```json
{
  "path": "docs/guide.md",
  "line": 3,
  "excerpt": "## Retries\nThe [retry] [policy] allows three attempts.\n",
  "section": "docs/guide.md#retries"
}
```

Use the returned handle to read exactly that section. Add `--parent` to read its enclosing section.

```sh
cx section 'docs/guide.md#retries' | jq -r '.body'
```

```markdown
## Retries
The retry policy allows three attempts.
```

### Where is the delay defined, and who calls it?

```sh
cx symbol retry_delay | jq '.hits[]'
```

```json
{
  "path": "src/lib.rs",
  "line": 1,
  "byte_column": 8,
  "name": "retry_delay",
  "kind": "function_item"
}
```

Here the declaration is on an ASCII line, so byte column 8 is also UTF-16 column 8. Use that position to find the enclosing caller.

```sh
cx callers src/lib.rs 1 8 | jq '{relationship_status, callers: [.result[] | {caller, path, line, col}]}'
```

```json
{
  "relationship_status": "partial",
  "callers": [
    {
      "caller": "send",
      "path": "src/lib.rs",
      "line": 3,
      "col": 24
    }
  ]
}
```

`partial` means other callers may exist. Check `truncated` in the full response to see whether the result limit was reached.

## More queries

```sh
cx content 'three attempts'             # Exact text with line and byte column
cx regex 'TODO|FIXME'                   # Regex over file contents
cx path-regex '(^|/)tests/'             # Regex over repository-relative paths
cx backlinks 'docs/guide.md#retries'    # Links to this heading
```

Run `cx help` for all commands or `cx help references` for a single command. Definition, reference, and caller queries take one-based lines and UTF-16 columns.

## License

Codeindex is MIT-licensed. The bundled Unicode tables carry their own notice in `src/domain/pattern/unicode/LICENSE-UNICODE`.
