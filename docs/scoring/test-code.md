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

Role queries use the captured source for the current generation. In a plain folder they use an in-memory capture. The role report runs on request. In a repository, indexing saves the ordinary score and any distinct test-adjusted score with each file. A warm score query reads the selected findings directly from the snapshot. When a file or Dart suite entry crosses the ordinary 70-line threshold, scoring can divide that measurement among recognized Dart registrations, Pester cases, Ruby tests, or pgTAP assertions. Setup hooks and the remaining suite code retain their own line counts. A 75-line hook still produces a finding even when every test is short. Overlapping registrations or a mismatch with the stored measurement keep the original finding.

## What counts as nesting in a test

Four nested suite declarations around a straight-line assertion should produce zero nesting findings. Four nested decisions inside one test or setup hook reach the depth threshold of four. Two separate stretches of depth two or greater in the same executable unit reach the nested-regions threshold. One such stretch in a test and one in its setup belong to separate units.

The scorer uses the language's ordinary control-flow measurement where that already handles suite structure. Test-specific rules recognize additional runner blocks such as Pester `Context`, Ruby `before`, and Kotlin `describe`. In web-platform harness pages it can measure JavaScript inside a test registration. In NixOS tests it can measure a literal, indented Python `testScript`. Generated code and dynamically registered custom runners can require runtime information.

## Conditional test expectations

A test can be shallow and still make its expected result hard to read. The `conditional test expectation` finding points to a local accumulator updated under multiple decisions and later compared with another value. For example, a test might increment a count under three independent conditions and then compare it with the size of the returned collection. Comparing an observed count with a fixed literal stays silent. Another recognized form uses one indexed sequence to choose how two alternative assertions check the corresponding element of a different sequence in a loop.

The rule uses written tests, local variables and recognized assertions. A finding identifies code to inspect. A helper that computes the expectation in another file can be invisible to this measurement. A Pester assertion written as a standalone conditional throw counts toward its case's branch complexity. A direct throw of a message as the entire failure arm can express a compound assertion without a separate compound-condition warning.

## How to check it

The role eval compares predictions at labelled source locations against real runner discovery, manually reviewed public repositories, and published Java method labels. It reports each language separately and compares against filename conventions. The original evaluation split by repository. Both published splits now serve as regression cases after their disagreements were inspected. The harness reports original labels and independently reviewed corrections separately. Run `python3 benchmarks/test_roles/eval.py --split dev` from the checkout to reproduce the development comparison. [TestRoutes dataset](https://doi.org/10.5281/zenodo.3741674)

AromaLIA labels ten test patterns across five language versions, including translations of the same originals. Those labels are relevant to a future evaluation of test-specific findings. [AromaLIA data](https://github.com/publiosilva/aroma-lia-evaluation-rp/tree/main/single-language-analysis/dataset-sheets/ground-truth)
