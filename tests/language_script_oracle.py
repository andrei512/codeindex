"""Run compiler-selected TypeScript navigation queries against the public CLI."""
import json
from pathlib import Path
import sys

from language_conformance import QuerySession, check_evidence, relationship_envelope, site_locations, verify_comparison


def query(session, op, anchor):
    proc = session.advanced(op, anchor)
    proc.check_returncode()
    answer = json.loads(proc.stdout)
    if error := relationship_envelope(answer, op) or check_evidence(answer, op):
        raise ValueError(f"{op} at {anchor}: {error}")
    return dict(status=answer["operation_status"],
                sites=list(site_locations(answer["result"], op, True, False)),
                truncated=answer["truncated"])


def compare(row, definition, references, known):
    target, use = row["target"], row["use"]
    present, truncated = use in references["sites"], references["truncated"]
    row.update(definition=definition, referencePresent=present, referenceTruncated=truncated,
               forwardMismatch=definition != dict(status="resolved", sites=[target], truncated=False))
    wrong = [site for site in references["sites"] if known.get(site) not in (None, target)]
    if wrong:
        row["wrongReverseSites"] = wrong
    row["reverseMismatch"] = references["status"] != "resolved" or bool(wrong) or not (present or truncated)


def verify_reverse_labels():
    row = dict(use="fixture.js:4:2", target="fixture.js:1:1")
    known = {row["use"]: row["target"], "fixture.js:6:2": "fixture.js:2:1"}
    forward = dict(status="resolved", sites=[row["target"]], truncated=False)
    def check(sites, truncated=False):
        compare(row, forward, dict(status="resolved", sites=sites, truncated=truncated), known)
        return row["reverseMismatch"]
    cases = (([row["use"], "fixture.js:6:2"], False, True),
             ([row["use"], "unknown.js:3:1"], False, False),
             ([], True, False))
    for sites, truncated, mismatch in cases:
        if check(sites, truncated) != mismatch:
            raise RuntimeError(f"reverse evidence disagrees for {sites}, truncated={truncated}")


def execute(binary, report, batch):
    verify_comparison()
    verify_reverse_labels()
    sessions = {project: QuerySession(binary, repo, batch["home"]) for project, repo in batch["bases"].items()}
    waves = batch["waves"]
    mismatches = 0
    report.parent.mkdir(parents=True, exist_ok=True)
    for i, wave in enumerate(waves, start=1):
        for row in wave:
            session = sessions[row["project"]]
            definition = query(session, "definition", row["use"])
            references = query(session, "references", row["target"])
            compare(row, definition, references, batch["witnesses"][row["project"]])
        bad = sum(row["forwardMismatch"] or row["reverseMismatch"] for row in wave)
        incomplete = sum(row["referenceTruncated"] and not row["referencePresent"] for row in wave)
        mismatches += bad
        print(wave[0]["family"], "wave", i, "samples", len(wave), "mismatched", bad, "uncertain reverse", incomplete)
        snapshot = dict(typescript=batch["typescript"], inventory=batch["inventory"], completed=i, waves=waves)
        report.write_text(json.dumps(snapshot, indent=2, ensure_ascii=False) + "\n")
    return int(mismatches > 0)


if __name__ == "__main__":
    sys.exit(execute(sys.argv[1], Path(sys.argv[2]), json.load(sys.stdin)))
