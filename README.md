# Codeindex

Codeindex installs `cx`, a local search CLI for coding agents. Run it anywhere inside a Git working tree. The first query builds an index under `~/.codeindex`. Later queries refresh the index against the working tree, including uncommitted edits and eligible untracked files.

## Install

```sh
cargo install --locked codeindex
```

To install from a checkout, run `cargo install --locked --path .`. The CLI targets macOS and Linux. On macOS, a watcher keeps the index warm for five minutes after the last request.

## Use

```sh
cx search 'retry policy'
cx regex 'TODO|FIXME'
cx path-regex '(^|/)tests/'
cx symbol RetryPolicy
cx references src/lib.rs 42 8
cx section 'docs/design.md#storage'
cx backlinks 'docs/design.md#storage'
```

Queries write JSON to stdout. Check `truncated` on hit lists and `relationship_status` for semantic coverage. Definition, reference and caller queries use one-based lines and UTF-16 columns. Run `cx help` for all commands or `cx help references` for a single command.

## License

Codeindex is MIT-licensed. The bundled Unicode tables carry their own notice in `src/domain/pattern/unicode/LICENSE-UNICODE`.
