# Codeindex

Codeindex gives coding agents a local, searchable map of a source folder. It installs one command, `cx`. An agent can search for a concept in the docs, jump to a declaration, and follow a call back to the code that uses it. Bare searches print matching source lines. Named queries return JSON with repository-relative paths and locations the agent can open next.

Before changing a retry policy, an agent needs to know where it is documented, which function implements the delay, and who calls that function. Those are three different questions about the same checkout. Codeindex indexes source text, paths, code relationships, and Markdown together so an agent can follow that trail across files.

Run a query anywhere inside the working tree. The first query builds the local index under `~/.codeindex`. Later queries refresh it against your working files, including uncommitted edits and eligible untracked files. There is no separate build command or server to configure.

Outside Git, run the same commands from the folder you want to search. Pattern searches scan the named files or directories directly. Ranked, symbol, reference, and Markdown queries build their query data in RAM and release it when the command exits. Each invocation sees the current files.

## Install

```sh
cargo install --locked codeindex
```

To install from a checkout, run `cargo install --locked --path .`. The CLI targets macOS and Linux. On macOS, a watcher keeps the index warm for five minutes after the last request.


For lookarounds and backreferences, use `cx -P PATTERN` with the system PCRE2-8 shared library installed. `cx --pcre2-version` checks availability. The default search engine is built into Codeindex.

## Maintainability score

Run `cx --score` for the repository, `cx --score src/` for a directory, or `cx --score src/main.rs` for a file. A file result explains each finding at its source location. For example, a Python function with five parameters and no other findings prints:

```text
example.py: 9.69/10 (2 lines)
example.py:1: inspect | parameters 5 (threshold 5)
```

Repository and directory scores are line-weighted averages. The output shows coverage and up to ten lowest-scoring files with findings. Add `--json` for every file and finding. Empty files, binaries, and unrecognized formats contribute no weight. Markdown contributes a separate measurement of section length to the same average.

The native index stores file-local measurements for every supported language. cx measures branch complexity (9), unit length in token-bearing lines (70), logical operators in a condition (2), distinct nested regions (2), structural nesting depth (4), parameters (5), and similar bodies (10 lines). Numbers in parentheses are inclusive reporting thresholds. In brace-based procedural languages, nesting follows control suites. Nested object and collection literals add no control depth. Go and JavaScript callback bodies have their own measurements. Python and Rust method parameter counts exclude recognized receivers. Build and configuration files can have named targets or blocks. Markdown uses nonblank lines per section. Duplication compares non-overlapping bodies within a file using a 87.5% Jaccard threshold on adjacent token pairs, counting repetitions. A score of 10 means no measured rule fired.

The numeric model is independently calibrated against published Python examples. Accuracy varies with the language and rule. [Public evaluation and reproduction instructions](benchmarks/README.md#maintainability-score-accuracy)

## Search from the shell

The examples below describe this checkout. The design target, including differences from current behavior, is in the [CLI invocation contract](docs/boxes/cli/invocation.md).

Use `cx PATTERN [PATH...]` for regular expressions. Use `-n` for line numbers, `-H` for filenames, `-F` for literal text, and `-A`, `-B`, or `-C` for context. Inside Git, ordinary searches use Codeindex's native index. A single explicit file with piped stdout prints source text by default.

```console
$ cx '^fn main' src/main.rs | head -1
fn main() -> std::process::ExitCode {
$ cx -nH '^fn main' src/main.rs
src/main.rs:1:fn main() -> std::process::ExitCode {
```

Use `cx -k 'retry policy'` for ranked passages, `cx --symbol NAME` for declarations, and `cx --reference NAME` for uses. These modes return `path:line:source`. Add `--column` for the byte column or `--json` for structured evidence. Partial-coverage notices go to stderr. Existing scripts can reach the advanced JSON commands through `cx --cx COMMAND`.

## Give an agent the quick start

Paste the output of `cx --cx intro` into the agent’s instructions once. It works from any directory. For example, to include it in a Codex CLI task:

```sh
codex exec "$(cx --cx intro)

Where is the retry policy defined, and who calls it?"
```

## From a question to the code

These examples use a small Rust client with a retry guide. These named commands write JSON directly. The optional `jq` filters show the fields an agent would use for its next step.

<details>
<summary>Create the example repository to try these queries</summary>

```sh
git init -q retry-demo
cd retry-demo
mkdir -p src docs
printf 'pub fn retry_delay() -> u64 { 50 }\n\npub fn send() -> u64 { retry_delay() }\n' > src/lib.rs
printf '# Client guide\n\n## Retries\nThe retry policy allows three attempts.\n' > docs/guide.md
printf '# Client\nSee [Retries](docs/guide.md#retries).\n' > README.md
```

</details>

### Where is the retry policy documented?

`-k` ranks passages with BM25. A Markdown hit includes the heading trail and a handle for the matching section.

```sh
cx -k 'retry policy' --json | jq '.hits[0] | {path, line, excerpt, section: .breadcrumbs[-1].handle}'
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
cx --cx section 'docs/guide.md#retries' | jq -r '.body'
```

```markdown
## Retries
The retry policy allows three attempts.
```

### Where is the delay defined, and who calls it?

```sh
cx --cx symbol retry_delay | jq '.hits[]'
```

```json
{
  "path": "src/lib.rs",
  "line": 1,
  "byte_column": 8,
  "col": 8,
  "name": "retry_delay",
  "kind": "function_item"
}
```

Ask for callers by name. For an indexed Rust inherent method, use `cx --cx callers Type::method`. When several declarations share a name, select a symbol hit and pass its `path`, `line`, and `col` from the repository root. The same forms work with `cx --cx references`.

```sh
cx --cx callers retry_delay | jq '{relationship_status, callers: [.result[] | {caller, path, line, col}]}'
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

## Find text and files

**Exact text.** Find the words that set the retry limit and get their byte column on the matching line.

```sh
cx --cx content 'three attempts' | jq -c '.hits[] | {path, line, byte_column}'
```

```json
{"path":"docs/guide.md","line":4,"byte_column":25}
```

**Content regex.** Find either spelling of the function name wherever it appears in the source.

```sh
cx --cx regex 'retry_(delay|timeout)' | jq -c '[.hits[] | {path, line, byte_column}]'
```

```json
[{"path":"src/lib.rs","line":1,"byte_column":8},{"path":"src/lib.rs","line":3,"byte_column":24}]
```

**Path queries.** Use a substring when you remember part of a name, an exact path to check membership, a regex for a naming rule, or a glob for a directory pattern. Each command below projects matching paths as a JSON array.

```console
$ cx --cx path guide | jq -c '[.hits[].path]'
["docs/guide.md"]
```

```console
$ cx --cx path-exact src/lib.rs | jq -c '[.hits[].path]'
["src/lib.rs"]
```

```console
$ cx --cx path-regex '^docs/.*\.md$' | jq -c '[.hits[].path]'
["docs/guide.md"]
```

```console
$ cx --cx glob '**/*.rs' | jq -c '[.hits[].path]'
["src/lib.rs"]
```

## Follow a symbol in either direction

**Definition.** Start on the call to `retry_delay` inside `send` and locate its declaration.

```sh
cx --cx definition src/lib.rs 3 24 | jq -c '[.result[] | {path, line, col}]'
```

```json
[{"path":"src/lib.rs","line":1,"col":8}]
```

**References.** Start on the declaration and locate uses of the symbol. Use `callers` from the walkthrough when you also want the enclosing function.

```sh
cx --cx references src/lib.rs 1 8 | jq -c '[.result[] | {path, line, col}]'
```

```json
[{"path":"src/lib.rs","line":3,"col":24}]
```

## Navigate Markdown

**Parent section.** Move up from the retry policy to its enclosing heading.

```sh
cx --cx section 'docs/guide.md#retries' --parent | jq -c '{title, handle, line}'
```

```json
{"title":"Client guide","handle":"docs/guide.md#client-guide","line":1}
```

**Backlinks.** Before renaming the heading, find the documents that link to it and the section containing each link.

```sh
cx --cx backlinks 'docs/guide.md#retries' | jq -c '[.hits[] | {path, line, handle}]'
```

```json
[{"path":"README.md","line":2,"handle":"README.md#client"}]
```

Run `cx --help` for all commands or `cx --cx help references` for a single command. Advanced positional JSON commands take one-based UTF-16 columns. Public `--reference FILE LINE COL` takes a one-based byte column.

## License

Codeindex is MIT-licensed. The bundled Unicode tables carry their own notice in `src/domain/pattern/unicode/LICENSE-UNICODE`.
