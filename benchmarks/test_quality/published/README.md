# Published detection labels

Use this development panel to evaluate whether a rule finds a published test pattern. Each label names a specific smell and its observation unit. The panel has 45,834 binary decisions. A negative for one smell says nothing about other problems, the author's willingness to refactor, the absolute score, or which test is easier to maintain.

```sh
python3 benchmarks/test_quality/published/panel.py
cargo build --locked --release
python3 benchmarks/test_quality/published/baseline.py
```

Python’s standard library is sufficient. The scripts download pinned research artifacts, verify digests, and keep labels, third-party sources, and the full CLI output under `target/test-quality/published`. Both check the local blind reservation list against development project names before materializing results or scoring sources. None of the studies’ original project repositories is cloned.

## AromaLIA

The checked-in metadata identifies 166 families originating in 105 Java and Python repositories. Each family contains a test in five languages: Java, Python, C#, JavaScript, and TypeScript. The 830 files have ten reviewed presence/absence decisions each. The normalized record includes original source URI, origin repository, translation status, fixture path, and family. Sample by origin repository and keep translations together. Conditional Test Logic has 234 positives out of 830 files. The authors enriched this collection for smells. Percentages describe the panel, not their frequency in ordinary code. The upstream source revision provides no explicit repository license; the loader fetches files from the authors. [Data and labeling protocol](https://github.com/publiosilva/aroma-lia-evaluation-rp/tree/d35f1bb9c2483d694c58b3a3bfddb5a800691507)

The baseline runs the real compatibility scorer on all 830 fixture files in one in-memory index. It observed 98 structural findings across 80 files. The other 750 files had no findings. No ordinary scorer rule has the same definition as a published AromaLIA rule. For example, branch complexity requires a threshold crossing while Conditional Test Logic records the presence of a qualifying construct. Mapping one to the other would give misleading precision and recall. Inspect the compact baseline and the individual JSON results before adding a candidate-specific comparison. [Measured baseline](baseline.json)

## Pontillo et al.

The original appendix has 9,633 rows with four manually classified smells per Java test. The combined oracle uses lowercased qualified method names. Some project/method keys repeat. One consistent label gets one vote per smell and method. Conflicts yield an omitted label, so a method can still have usable labels for its other smells. Of 9,428 distinct method keys, seven belong to a project absent from the source-pin manifest (`aletheia`). The loader excludes those seven and 150 conflicting decisions. The remaining 9,421 methods supply 37,534 usable decisions across 65 projects. The normalized record retains the original method key and the upstream project revision, which may be abbreviated. Source-file and byte coordinates would need a separate validated join before a CLI comparison. These methods contribute no CLI baseline hits or misses here. The appendix carries the GPL v3 license. [Paper](https://doi.org/10.1007/s10664-023-10436-2) · [Oracle and terms](https://github.com/darioamorosodaragona-tuni/ML-Test-Smell-Detection-Online-Appendix/tree/6c74556fe8332e5e0864b8019cd8ddee2979af84)

The checked-in coverage records each rule’s positive and negative counts, exclusions, and normalized content digest. Source pins and the deterministic fixture-bundle digest allow independent reruns. A fresh binary can have a different executable digest even when the results match. [Coverage](coverage.json) · [Source pins](sources.json)
