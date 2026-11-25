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


def names(node):
    for child in node.named_children:
        if child.type == "function_declaration":
            continue
        if child.type == "call_expression":
            head = child.child_by_field_name("function") or child.named_children[0]
            # Only simple calls and qualified member calls have a comparable name.
            if head.type in {"identifier", "simple_identifier", "selector_expression", "navigation_expression"}:
                match = re.search(rb"([A-Za-z_][A-Za-z_0-9]*)$", head.text)
                if match:
                    yield match[1].decode()
        yield from names(child)


def main():
    blind = {row["repository"].lower() for row in json.loads((HERE / "blind.lock.json").read_text())}
    rows = []
    for line in (HERE / "verbosity_units.jsonl").read_text().splitlines():
        r = json.loads(line)
        if r["split"] != "dev" or r["repository"].lower() in blind:
            raise ValueError("outside development panel")
        data = (HERE.parents[1] / "target/test-role-eval/blobs" / r["sha256"]).read_bytes()
        if hashlib.sha256(data).hexdigest() != r["sha256"]:
            raise ValueError("source hash mismatch")
        src = data[r["start_byte"]:r["end_byte"]]
        root = get_parser(r["language"]).parse(src).root_node
        unit = next(n for n in root.named_children if n.type == "function_declaration")
        if unit.child_by_field_name("name").text.decode() != r["unit"]:
            raise ValueError("method anchor mismatch")
        calls = list(names(unit))
        seen = collections.defaultdict(list)
        for i in range(len(calls) - 2):
            seq = tuple(calls[i:i+3])
            if len(set(seq)) == 3 and (not seen[seq] or i >= seen[seq][-1]+3):
                seen[seq].append(i)
        rows.append(dict(unit=r["unit"], expected=r["repetition_hypothesis"],
                         matches=[seq for seq, hits in seen.items() if len(hits) >= 3]))
    print(json.dumps(rows, indent=2))


if __name__ == "__main__":
    main()
