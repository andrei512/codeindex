---
page-type: landing
---
# Command-line interface

The CLI owns the contract a coding agent sees when it invokes Codeindex. This includes command names and arguments, help, the format of results on stdout, diagnostics on stderr, and exit status.

The design gives an agent four entry points. Use `cx PATTERN` for ordinary ripgrep-style text search, `cx -k 'topic'` for ranked passages, `cx --symbol NAME` for declarations and `cx --reference NAME` for uses. Codeindex owns the search in every mode. The invocation contract records the target behavior, current gaps and the tests that distinguish them.

## Topics

### Design

- [Invocation contract](invocation.md) — Maps arguments to typed requests, output and exit behavior.
