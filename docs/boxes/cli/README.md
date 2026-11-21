---
page-type: landing
---
# Command-line interface

The CLI owns the contract a coding agent sees when it invokes Codeindex. This includes command names and arguments, help, the format of results on stdout, diagnostics on stderr, and exit status.

For a callers request, the CLI validates the arguments and asks the application to refresh and pin the working directory’s repository, then passes the pin and typed request to query execution. Query execution determines the callers, their order, and coverage. The CLI renders that answer. On macOS a live watcher executes the query with its pinned generation and returns the answer over a private socket. Foreground execution uses the same operations.

Run `cx search "retry"` from a repository to build the index and return ranked passages. Subsequent queries reuse the published generation while its inputs and extraction rules match. Definition, references and callers responses report partial coverage while the native language profiles are being verified.

## Topics

### Design

- [Invocation contract](invocation.md) — Maps arguments to typed requests, output and exit behavior.
