# Controlled test-score pairs

The manifest describes two challenges translated into Python, Rust, Go, JavaScript, TypeScript and Java. Each variant lives in a separate directory so the harness can stage it at the same filename and compare the file score. Copy the indicated subject beside the test file when running it. Only the test file is a score target.

- **Fixture layout.** Compact and expanded files differ only in whitespace between the same six pairs of literals. The declared score expectation is a tie. A difference measures sensitivity to formatting.
- **Conditional logic.** One test computes expectations by branching on input. The alternative gives explicit expected results in two focused tests. A higher score for the alternative is a hypothesis. These agent-authored judgments supply no independent evidence of maintainers’ preferences.

All twelve manifest records belong to one synthetic family and the development split. Report the two challenges separately, with language-level outcomes. Keep native JS and TypeScript observations visible under their shared JS profile. Mutant detection checks the fixture construction; it supplies no evidence about the relative readability of the tests.

## Verify the fixtures

Run from the repository root:

```sh
python3 benchmarks/test_quality/pairs/verify.py --language python --language rust
python3 benchmarks/test_quality/pairs/verify.py --docker go --docker java --junit-dir /path/to/junit-jars
```

The verifier uses unittest, the Rust test harness, Go testing, Node's test runner (with native TypeScript support), and JUnit 4. Each variant must visit the same six inputs exactly once and pass. Two separate executions inject a defect into the negative and nonnegative paths, respectively. Each must reach the affected input and exit with the runner's failure status. These six input points fully enumerate the scenarios in these fixtures; two injected faults exercise representative failures.

For Java, supply JUnit 4.11 and Hamcrest Core 1.3; the runner verifies both SHA-256 hashes. The docker switches run the Go and JDK toolchains by digest and isolate them from the network after image retrieval. The remaining languages use locally installed toolchains. [JUnit 4.11](https://repo.maven.apache.org/maven2/junit/junit/4.11/junit-4.11.jar) · [Hamcrest Core 1.3](https://repo.maven.apache.org/maven2/org/hamcrest/hamcrest-core/1.3/hamcrest-core-1.3.jar)
