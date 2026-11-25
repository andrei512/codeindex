---
page-type: concept
---
# Recognizing test code

A file can contain production code, a runnable test, and the helper that constructs its fixture. Use `cx --score --roles FILE` to see which role Codeindex assigns to each part and the evidence for that assignment. A directory argument shows the same breakdown for its text files.

## Reading a result

For a Rust library with a test module, the output can look like this:

```text
src/lib.rs: production (ordinary source path)
src/lib.rs:3-7: test-support (test-only compilation)
src/lib.rs:5-6: test (test attribute)
```

The first line supplies the default. A region overrides it. Here the helper in the module inherits `test-support` and the function with `#[test]` gets `test`. A call from that function into production code leaves the callee's role unchanged. Add `--json` for one-based inclusive line ranges and zero-based byte offsets with an exclusive end. At a byte covered by several regions, use the last matching region. Byte offsets distinguish neighboring declarations on the same line.

## What counts as evidence

Every code-language profile and Markdown uses path conventions to identify test trees, fixture files, and ordinary sources. The classifier can return `unknown` for ambiguous example paths and unrecognized file types. An ordinary path is a default inference. It cannot prove that a file ships in a release.

Language rules refine that default where Codeindex recognizes a test declaration, framework registration, or test-only scope. JavaScript can follow a runner through local imports and wrappers. Java and Swift use the class that owns the method to distinguish a discovered test from a helper. C and C++ can use macro definitions and CMake test registrations. Declarative evidence also includes Starlark suite lists, web-platform HTML test registrations, Make test recipes and PostgreSQL regression targets. Assets consumed by tests, such as a CSS file or Dockerfile, can be test support without containing a runnable test. An empty region list means the file default applies throughout the result. Custom runners, generated registrations, and helpers whose only evidence comes from another build target can require project information beyond the signals available to the classifier.

Role queries use the captured source for the current generation. In a plain folder they use an in-memory capture. Classification runs on request. The numeric maintainability score continues to use the same rules for all roles. Deciding which findings are actionable in tests needs its own evaluation.

## How to check it

The role eval compares predictions at labelled source locations against real runner discovery, manually reviewed public repositories, and published Java method labels. It reports each language separately and compares against filename conventions. The original evaluation split by repository. Both published splits now serve as regression cases after their disagreements were inspected. The harness reports original labels and independently reviewed corrections separately. Run `python3 benchmarks/test_roles/eval.py --split dev` from the checkout to reproduce the development comparison. [TestRoutes dataset](https://doi.org/10.5281/zenodo.3741674)

AromaLIA labels ten test patterns across five language versions, including translations of the same originals. Those labels are relevant to a future evaluation of test-specific findings. [AromaLIA data](https://github.com/publiosilva/aroma-lia-evaluation-rp/tree/main/single-language-analysis/dataset-sheets/ground-truth)
