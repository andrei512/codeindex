"""Validate and compare public symbol and relationship replies for fixture oracles."""
from collections import Counter
from dataclasses import dataclass
from functools import lru_cache
from itertools import islice
import json


def expectation(op, subject, expected, status=None):
    if status is None:
        status = "" if op == "symbol" else "resolved"
    return dict(op=op, subject=subject, expected=expected, status=status, subset=False)


def query_exit(answer, op, rows):
    if op not in ("symbol", "references"):
        return 0
    if answer.get("operation_status") in ("unsupported", "unindexed"):
        return 2
    return int(not rows)


def exit_difference(exit_code, answer, op, rows):
    if exit_code is None:
        return {}
    expected = query_exit(answer, op, rows)
    return {} if exit_code == expected else {"exit": [expected, exit_code]}


def truncated_count(answer, rows):
    if answer["truncated"] and len(rows) != 50:
        return {"truncated_count": [50, len(rows)]}
    return {}


def symbol_error(answer, name, exit_code=None):
    hits = answer["hits"]
    valid_envelope = (
        answer.get("mode") == "symbol",
        type(answer.get("truncated")) is bool,
        isinstance(hits, list),
    )
    if not all(valid_envelope):
        return {"wrong_envelope": True}
    if diff := truncated_count(answer, hits):
        return diff
    return hit_error(hits, name) or exit_difference(exit_code, answer, "symbol", hits)


def hit_key(hit, name):
    if name is not None and hit.get("name") != name:
        raise ValueError({"returned_name": hit.get("name")})
    if not valid_coordinates(hit, ("line", "col", "byte_column")):
        raise ValueError({"invalid_coordinates": hit})
    return hit["path"], hit["line"], hit["byte_column"]


def hit_error(hits, name):
    previous = ()
    for hit in hits:
        try:
            key = hit_key(hit, name)
        except ValueError as error:
            return error.args[0]
        if key < previous:
            return {"out_of_order": hit}
        previous = key
    return {}


def valid_coordinates(row, keys):
    return all(type(row.get(key)) is int and 1 <= row[key] <= 2**53 - 1 for key in keys)


def check_response(proc, query, phase):
    answer = json.loads(proc.stdout)
    op = query["op"]
    if op == "symbol":
        rows = answer["hits"]
        diff = symbol_error(answer, query["subject"]) or compare_rows(answer, rows, query)
    else:
        rows = answer["result"]
        diff = compare_rows(answer, rows, query)
    if phase == "reopen" and answer.get("reused") is not True:
        diff["reopen"] = "expected to reuse the published generation"
    diff.update(exit_difference(proc.returncode, answer, op, rows))
    if op != "symbol" and (error := check_evidence(answer, op)):
        diff["evidence"] = error
    return diff


def binding_targets(binding):
    certainty, group = binding["certainty"], binding["targets"]
    check_target_names(group)
    if not {"unresolved": not group, "exact": len(group) == 1, "candidates": bool(group)}.get(certainty):
        raise ValueError("invalid binding certainty or target count")
    return group


def check_target_names(group):
    if not isinstance(group, list) or not all(isinstance(target, str) for target in group):
        raise ValueError("expected a list of symbol names in binding targets")
    if any(a >= b for a, b in zip(group, islice(group, 1, None))):
        raise ValueError("binding targets must be distinct and sorted")


def ordered_groups(items, key, label):
    previous = None
    for item in items:
        current = key(item)
        if previous is not None and current <= previous:
            reason = "duplicate" if current == previous else "out-of-order"
            raise ValueError(f"{reason} {label}: {current}")
        previous = current
        yield item


def binding_key(binding):
    targets = binding_targets(binding)
    return binding.get("role", ""), binding["certainty"], targets


def context_targets(contexts):
    ordered = ordered_groups(contexts, lambda item: item["context"], "translation context")
    return {
        ctx["context"]: {name for binding in ordered_groups(ctx["bindings"], binding_key, "binding group")
                         for name in binding["targets"]}
        for ctx in ordered
    }


RELATIONSHIP_ROLES = {
    "definition": "definition",
    "references": "reference",
    "callers": "call_target",
}


def selected_evidence(item, role, targets):
    if item["role"] == role:
        target = item["target"]
        context = item["context"]
        if target in targets.get(context, ()):
            return context, target
    raise ValueError("evidence disagrees with the operation or selected binding")


def checked_evidence_key(item, rows, targets, role):
    hit = evidence_hit(item, rows)
    context, target = selected_evidence(item, role, targets)
    if role != "call_target" and rows[hit]["symbol"] != target:
        raise ValueError("hit and evidence name different targets")
    certainty = item.get("certainty")
    if certainty not in ("exact", "candidates"):
        raise ValueError("invalid supporting edge certainty")
    return hit, context, certainty, target


def evidence_hit(item, rows):
    hit = item["hit"]
    if type(hit) is not int or not 0 <= hit < len(rows):
        raise ValueError("evidence points outside the result list")
    return hit


def evidence_keys(answer, op, targets):
    """Yield validated edges in their reported order; each must refer to its selected binding."""
    rows = answer["result"]
    role = RELATIONSHIP_ROLES[op]
    for item in answer["evidence"]:
        yield checked_evidence_key(item, rows, targets, role)


def check_evidence(answer, op):
    targets = context_targets(answer["contexts"])
    previous = (-1, "", "", "")
    try:
        for identity in evidence_keys(answer, op, targets):
            if identity <= previous:
                return "duplicate relationship evidence" if identity == previous else "relationship evidence is out of order"
            if identity[0] > previous[0] + 1:
                return "a reported location has no supporting evidence"
            previous = identity
    except ValueError as error:
        return str(error)
    if previous[0] + 1 != len(answer["result"]):
        return "a reported location has no supporting evidence"


def site_location(row, op, with_col, check_label):
    values = [row["line"], row["col"]]
    if op != "symbol" and not valid_coordinates(row, ("line", "col")):
        raise ValueError(f"expected positive integer coordinates, got {values}")
    site = ":".join([row["path"], *map(str, values if with_col else values[:1])])
    if check_label:
        label = row["kind" if op == "symbol" else "caller"]
        return f"{site}|{label}"
    return site


def site_locations(rows, op, with_col, check_label):
    return (site_location(row, op, with_col, check_label) for row in rows)


def compare_status(diff, answer, status):
    status, _, coverage = status.partition("/")
    if coverage and {ctx["coverage"] for ctx in answer["contexts"]} != {coverage}:
        diff["coverage"] = [coverage, [ctx["coverage"] for ctx in answer["contexts"]]]
    if status and status != answer.get("operation_status"):
        diff["status"] = [status, answer.get("operation_status")]


def relationship_envelope(answer, op):
    diff = {key: [value, answer.get(key)] for key, value in (
        ("mode", op), ("position_encoding", "one-based UTF-16")) if answer.get(key) != value}
    if type(answer.get("truncated")) is not bool:
        diff["truncated"] = "expected a boolean"
    else:
        diff.update(truncated_count(answer, answer["result"]))
    return diff


@dataclass(frozen=True)
class ExpectedLocations:
    counts: Counter
    with_column: bool
    with_label: bool

    @classmethod
    @lru_cache(maxsize=1024)
    def parse(cls, spelling):
        sites = [] if spelling == "-" else spelling.split(",")
        return cls(Counter(sites), any(len(s.rsplit(":", 2)) == 3 for s in sites),
                   any("|" in s for s in sites))

    def difference(self, rows, op, subset):
        got = self.observe(rows, op)
        return self.difference_from(got, subset)

    def observe(self, rows, op):
        return Counter(site_locations(rows, op, self.with_column, self.with_label))

    def difference_from(self, got, subset):
        diff = {}
        if missing := self.counts - got:
            diff["missing"] = list(missing.elements())
        if not subset and (extra := got - self.counts):
            diff["unexpected"] = list(extra.elements())
        return diff


def compare_rows(answer, rows, query):
    op = query["op"]
    subset = query["subset"]
    diff = ExpectedLocations.parse(query["expected"]).difference(rows, op, subset)
    if op != "symbol":
        diff.update(relationship_envelope(answer, op))
    if answer.get("truncated") is True and not subset:
        diff["truncated"] = True
    compare_status(diff, answer, query["status"])
    return diff


def verify_comparison():
    answer = {"result": [{"path": "a", "line": 3, "col": 4}], "operation_status": "resolved",
              "contexts": [{"coverage": "partial"}], "mode": "references",
              "position_encoding": "one-based UTF-16", "truncated": False}
    def difference(answer):
        return compare_rows(answer, answer["result"], expectation("references", "", "a:3:4", "resolved/partial"))
    from unittest import TestCase
    verdict = TestCase()
    for change, reasons in (
        ({}, set()), ({"result": []}, {"missing"}),
        ({"result": [{"path": "a", "line": 3, "col": 5}]}, {"missing", "unexpected"}),
        ({"contexts": [{"coverage": "complete"}]}, {"coverage"}),
        ({"truncated": True}, {"truncated", "truncated_count"}),
        ({"truncated": 0}, {"truncated"}), ({"mode": "callers"}, {"mode"}),
        ({"position_encoding": "bytes"}, {"position_encoding"}),
    ):
        verdict.assertEqual(set(difference({**answer, **change})), reasons, change)
    for malformed in ({"line": "3"}, {"col": True}, {"col": 0}):
        row = {**answer["result"][0], **malformed}
        with verdict.assertRaisesRegex(ValueError, "positive integer coordinates"):
            compare_rows(answer, [row], expectation("references", "", "a:3", "resolved/partial"))

    page = {**answer, "result": answer["result"] * 50, "truncated": True}
    verdict.assertEqual(relationship_envelope(page, "references"), {})
    verdict.assertEqual(relationship_envelope({**page, "result": []}, "references"), {"truncated_count": [50, 0]})

    graph = {
        "result": [{"symbol": "target"}],
        "contexts": [{"context": "c", "bindings": [{"certainty": "exact", "targets": ["target"]}]}],
        "evidence": [{"hit": 0, "context": "c", "target": "target", "role": "reference", "certainty": "exact"}],
    }
    malformed_binding = {"certainty": "candidates", "targets": ["target", None]}
    with verdict.assertRaisesRegex(ValueError, "list of symbol names"):
        context_targets([{"context": "c", "bindings": [malformed_binding]}])
    with verdict.assertRaisesRegex(ValueError, "duplicate translation context"):
        context_targets(graph["contexts"] * 2)
    edge = graph["evidence"][0]
    distinct = [{**edge, "certainty": "candidates"}, edge]
    for change, reason in (
        ({"evidence": distinct}, None),
        ({"evidence": list(reversed(distinct))}, "relationship evidence is out of order"),
        ({"evidence": []}, "a reported location has no supporting evidence"),
        ({"evidence": [edge, edge]}, "duplicate relationship evidence"),
        ({"evidence": [{**edge, "certainty": "unknown"}]}, "invalid supporting edge certainty"),
        ({"contexts": []}, "evidence disagrees with the operation or selected binding"),
        ({"evidence": [{**edge, "hit": -1}]}, "evidence points outside the result list"),
        ({"evidence": [{**edge, "hit": False}]}, "evidence points outside the result list"),
        ({"evidence": [{**edge, "role": "definition"}]}, "evidence disagrees with the operation or selected binding"),
        ({"contexts": [{**graph["contexts"][0], "context": "other"}]}, "evidence disagrees with the operation or selected binding"),
        ({"result": [{"symbol": "decoy"}]}, "hit and evidence name different targets"),
    ):
        verdict.assertEqual(check_evidence({**graph, **change}, "references"), reason, change)
