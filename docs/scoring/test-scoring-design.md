---
page-type: concept
---
# Designing a maintainability score for tests

A useful test warning points to code that makes a behavior harder to understand, change, or diagnose when it fails. The scorer uses the original length threshold and can attribute lines to recognized test cases and setup hooks. This design defines which changes to evaluate and what evidence would justify changing the number that agents see.

## Which rules earn an experiment?

**Conditional logic in a test.** Measure decisions in the executable test body and locate them. Contrast a test that uses branches to choose several different expectations with a parameterized test whose rows all exercise the same check. Include explicit framework skips and environment guards. Spadini et al. found a strong correlation between their conditional-logic severity and developers’ perceived maintenance impact (Spearman 0.753, 36 responses). Whether counting decisions differently improves on the existing complexity rules is an experiment. A branch that can silently skip verification raises a separate correctness question. [Severity study, Table 5](https://sback.it/publications/msr2020.pdf)

**Test length.** Compare the current unit-line measure with a candidate that counts executable setup, action, and verification lines. For the candidate, exclude continuation lines containing only literal fixture values; retain calls and expressions embedded in the same initializer. Use paired examples where adding input rows preserves the behavior and readability of the test. The published verbosity measure tracked perceived impact (0.679, 51 responses). The study did not validate this executable-line variant. [Severity study, Table 5](https://sback.it/publications/msr2020.pdf)

**Assertion and call counts.** Several assertions may describe one behavior and produce precise framework diagnostics. Counting calls also includes setup. The corresponding published measures had weaker agreement with developers (0.350 for assertion roulette and 0.342 for eager tests). A controlled student debugging study still found a cost in some settings. Evaluate whether someone can identify the failed expectation from the failure output. Assertion count, missing custom messages, and number of callees alone earn no new penalty. [Severity study](https://sback.it/publications/msr2020.pdf) · [Debugging experiment](https://arxiv.org/abs/2303.04234)

## Where does a rule apply?

Assign a finding to its executable unit using byte ranges from the same source generation. A Rust test and its helper can share a file or even a line. A helper keeps the ordinary structural rules unless an independently validated helper-specific rule applies. Fixture data receives no new test-specific penalty. Existing findings outside supported test units retain their ordinary treatment. The role label alone cannot establish that a helper is data, that two duplicated bodies serve the same purpose, or that a callback is a runnable test.

Compare both scoring policies on one pinned input. The hidden eval flag `cx --score --ignore-test-rules` selects the frozen role-independent formula and findings, including directory weights. A production-only input must yield identical results in both modes. Current stored findings include threshold crossings and source lines. A candidate that changes the measured unit needs its underlying byte-range evidence. For an eligible unit, a validated replacement contributes once in place of its corresponding baseline measurement. Aggregate the selected measurements once with the file formula; averaging separate test and production scores would change mixed-file results. Check same-line ownership, full finding output, and directory aggregation against the frozen binary. Reuse measurements during scoring and measure fresh-build and query cost before persisting additional facts.

## What earns a change to the score?

Keep three answers separate in the eval: whether a pattern occurs at a specific location, whether a reviewer can explain a maintenance cost there, and which of two behavior-equivalent tests is easier to maintain. A decision to defer a fix may reflect effort or competing priorities. Record that reason separately from a false alarm. Human-annotated test-smell datasets supply detection labels. Numeric calibration requires independent judgments of impact. [Manual detector evaluation](https://link.springer.com/article/10.1007/s10664-022-10207-5) · [AromaLIA evaluation data](https://github.com/publiosilva/aroma-lia-evaluation-rp)

The published replications do not supply a universal scoring formula. A proposed measurement earns a penalty when the blind evaluation demonstrates useful warnings and better rankings. The baseline remains available as a compatibility check.

## Topics

### Evaluation

- [Evaluate a proposed test scoring rule](evaluate-test-rules.md) defines how to collect evidence, compare both policies, and accept or reject a frozen candidate.
