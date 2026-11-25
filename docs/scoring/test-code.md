---
page-type: concept
---
# Recognizing test code

A runnable test, a helper used to construct its fixture, and the production function under test have different roles. They can share a file. Establishing those roles is a separate research question from deciding whether repeated setup or a long assertion sequence deserves a warning.

## What cx knows today

The scorer applies its language rules to each recognized unit. The numeric evaluation harness also groups paths such as `tests/checkout.py` under a test-path label. Other paths go into a separate bucket. These are evaluation statistics. The index does not currently assign test roles or apply role-specific score penalties. In particular, a Rust module can contain both a public function and its unit tests, and a Python test file can contain a helper with its own substantial control flow.

## What the public labels establish

[TestRoutes](https://doi.org/10.5281/zenodo.3741674) identifies selected Java test cases, their helpers and the production methods on their execution routes. It can check method roles, including the difference between setup and the code being tested. Its annotations cover selected routes. Treating every unlisted method as production would create false labels.

[AromaLIA](https://github.com/publiosilva/aroma-lia-evaluation-rp/tree/main/single-language-analysis/dataset-sheets/ground-truth) instead labels ten test patterns. Its five language versions include translations of the same originals. It can check consistency of a finding across languages. Recognizing a pattern does not establish that a developer should refactor it. These datasets support different evaluations. Evidence for recognizing test code across every supported language is still open.
