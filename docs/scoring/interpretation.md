---
page-type: essentials
---
# From a finding to a score

A score explains which file-local measurements crossed their thresholds. The index keeps the evidence so a query can show both the number and its cause from the same version of the source.

## Read a file finding

Suppose a Python function named `inspect` declares five ordinary parameters. With no other findings its file prints:

```text
example.py: 9.69/10 (2 lines)
example.py:1: inspect | parameters 5 (threshold 5)
```

Five is the measured argument count. The threshold is inclusive. The second line identifies what to inspect even if another finding in the same file changes the overall score. Parameter counts, branch complexity, unit length, compound conditions, nested regions, nesting depth and similar bodies each measure a different property. Language rules determine which tokens belong to a unit. For example, braces in a Lua table literal do not end the enclosing function. Markdown measures the length of sections delimited by headings.

During code analysis the scorer consumes syntax evidence and records the rule, unit, line and value for each finding. The query derives the file score from these saved values. The deepest function and the presence of other deeply nested functions both affect the penalty. A score of ten means that no measured rule fired. It gives no information about whether the program satisfies its requirements.

## Read a directory score

`cx --score src/` weights each file score by its scorable line count. Two files of 90 and 10 lines scoring 10 and 5 produce a mean of 9.5. Empty and unsupported files contribute zero weight. The text summary lists up to ten low-scoring files with findings. Use `cx --score --json src/` for the individual records, including coverage for files without a score.

## Judge the measurement

Three checks answer different questions. Numeric evaluation compares the file score against a published reference. Finding evaluation asks whether the same rule fired at the same line. Constructed boundary cases check that each supported profile handles particular syntax at its threshold. A good numeric match can hide a missing finding and an extra finding whose effects cancel. Read the per-rule disagreements when deciding whether a parser measurement is trustworthy.

## Topics

### Calculation

- [File score calculation](model.md) gives the thresholds, aggregation formula and commands to reproduce the development evaluation.

### Working with a repository

- [Refactor a repository using score findings](refactor.md) shows how to choose work, verify a batch and account for the last few findings.
