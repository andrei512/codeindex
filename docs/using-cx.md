---
page-type: task
---
# Follow a change from its policy to its callers

Use this walkthrough when a change starts as a question in ordinary language. It follows a retry policy from prose to the function that implements it, then to the code that calls it. The following small checkout makes the commands and results reproducible.

```sh
work=$(mktemp -d)
cd "$work"
git init -q
mkdir -p src docs
printf 'pub fn retry_delay() -> u64 { 50 }\npub fn send() -> u64 { retry_delay() }\n' > src/lib.rs
printf '# Guide\n\n## Retries\n\nThe retry policy allows three attempts.\n' > docs/guide.md
```

## Find the policy

```sh
cx -k 'retry policy' --json
```

Look for a passage that explains when a retry happens. Ranked search uses BM25, so a hit can contain the words in a different order. A Markdown result carries its heading trail. Given the handle `docs/guide.md#retries`, read the whole section with `cx --cx section 'docs/guide.md#retries'`. Before moving that heading, look for incoming links with `cx --cx backlinks 'docs/guide.md#retries'`.

## Find the implementation and its uses

```sh
cx --symbol retry_delay
cx --reference retry_delay
```

The first command shows declarations. The second accepts a unique name and shows its recorded uses. When several declarations share the name, choose the intended declaration and supply its path, one-based line and one-based UTF-8 byte column to `--reference`. Read the coverage diagnostic on stderr. If relationships are partial, a text search such as `cx -n 'retry_delay' .` can uncover additional candidates for inspection.

Inside Git, the first indexed query creates a checkpoint and later queries refresh it against the working files. From a plain directory the same ranked and semantic queries build what they need in memory for that invocation. To search the current directory explicitly when a runner supplies piped standard input, pass `.` to the ordinary text search.

The [CLI invocation contract](boxes/cli/invocation.md) gives the coordinate and exit-status rules for scripting these commands.
