"""Development-only probe: three different calls in sequence repeating three times."""
# /// script
# dependencies = ["tree-sitter-language-pack==0.9.0","tree-sitter==0.27.0"]
# ///
import collections
import hashlib
import json
import re
from pathlib import Path

from tree_sitter_language_pack import get_parser

HERE = Path(__file__).resolve().parent


def call_name(node):
    head = node.child_by_field_name("function") or node.named_children[0]
    if head.type not in {"identifier", "simple_identifier", "selector_expression", "navigation_expression"}:
        return None
    match = re.search(rb"([A-Za-z_][A-Za-z_0-9]*)$", head.text)
    return match[1].decode() if match else None


def names(node):
    for child in node.named_children:
        if child.type == "function_declaration":
            continue
        if child.type == "call_expression":
            name = call_name(child)
            if name:
                yield name
        yield from names(child)


def repeated_triples(calls):
    # Equal triples with three distinct names cannot overlap: shifting by
    # one or two calls would require one of those names to repeat.
    counts = collections.Counter()
    for seq in zip(calls, calls[1:], calls[2:]):
        if len(set(seq)) == 3:
            counts[seq] += 1
    return [seq for seq, count in counts.items() if count >= 3]


def development_unit(row, blind):
    if row["split"] != "dev" or row["repository"].lower() in blind:
        raise ValueError("outside development panel")
    data = (HERE.parents[1] / "target/test-role-eval/blobs" / row["sha256"]).read_bytes()
    if hashlib.sha256(data).hexdigest() != row["sha256"]:
        raise ValueError("source hash mismatch")
    src = data[row["start_byte"]:row["end_byte"]]
    root = get_parser(row["language"]).parse(src).root_node
    unit = next(n for n in root.named_children if n.type == "function_declaration")
    check_unit_anchor(unit, row["unit"])
    return unit


def check_unit_anchor(unit, name):
    if unit.child_by_field_name("name").text.decode() != name:
        raise ValueError("method anchor mismatch")


def main():
    blind = {row["repository"].lower() for row in json.loads((HERE / "blind.lock.json").read_text())}
    rows = []
    for line in (HERE / "verbosity_units.jsonl").read_text().splitlines():
        r = json.loads(line)
        unit = development_unit(r, blind)
        calls = list(names(unit))
        rows.append(dict(unit=r["unit"], expected=r["repetition_hypothesis"],
                         matches=repeated_triples(calls)))
    print(json.dumps(rows, indent=2))


if __name__ == "__main__":
    main()
