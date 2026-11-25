"""Prepare a separate, fixture-only development panel from Pygments' lexer runner.

tests/examplefiles/conftest.py collects inputs; tests/conftest.py reads their
bytes as data and compares lexer output against a sibling golden file. A source
file containing assertions can itself be a test fixture in this project.
"""
import collections
import hashlib
import json
import tarfile
import urllib.request
from pathlib import Path

REV = "156d85fdd18d34427fe893527e71da91dcfb0eb5"
ROOT = Path(__file__).resolve().parents[2]
CACHE = ROOT / "target/test-role-eval"
ARCHIVE = CACHE / "research/pygments.tgz"
# Use the upstream lexer's identity. Ambiguous filenames do not determine gold.
LEXERS = {lang: lang for lang in "rust python java kotlin swift lua dart go c cpp csharp css html mako perl php powershell procfile".split()}
LEXERS.update({"js":"js","jsx":"js","typescript":"js","tsx":"js","bash":"shell","bat":"batchfile",
               "cmake":"cmake","postgresql":"sql","sqlite3":"sql","md":"markdown","markdown":"markdown",
               "make":"makefile","protobuf":"proto","objective-c":"objectivec","docker":"dockerfile",
               "rb":"ruby","nixos":"nix","nasm":"assembly","ca65":"assembly","tasm":"assembly","mips":"assembly",
               "terraform":"hcl"})

def main():
    ARCHIVE.parent.mkdir(parents=True, exist_ok=True)
    (CACHE / "blobs").mkdir(exist_ok=True)
    if not ARCHIVE.exists():
        urllib.request.urlretrieve(f"https://codeload.github.com/pygments/pygments/tar.gz/{REV}", ARCHIVE)
    rows = []
    with tarfile.open(ARCHIVE) as tar:
        prefix = f"pygments-{REV}/"
        files = {m.name.removeprefix(prefix): m for m in tar if m.name.startswith(prefix) and m.isfile()}
        for path, member in sorted(files.items()):
            parts = path.split("/")
            if (len(parts) != 4 or parts[:2] != ["tests", "examplefiles"]
                    or parts[2] not in LEXERS or path + ".output" not in files):
                continue
            data = tar.extractfile(member).read()
            if not data or b"\0" in data:
                continue
            try:
                lines = data.decode().splitlines()
            except UnicodeDecodeError:
                continue
            gold = [i for i, line in enumerate(lines,1) if line.strip()]
            if not gold:
                continue
            digest = hashlib.sha256(data).hexdigest()
            (CACHE / "blobs" / digest).write_bytes(data)
            rows.append(dict(language=LEXERS[parts[2]], repository="pygments/pygments",revision=REV,
                path=path,sha256=digest,split="dev",labels=[[gold[len(gold)//2],"test-support",
                "LexerSeparateTestItem reads this file as input and compares tokens with its sibling .output golden."]]))
    manifest = ROOT / "benchmarks/test_roles/lexer_inputs.jsonl"
    text = "".join(json.dumps(r,sort_keys=True,ensure_ascii=False)+"\n" for r in rows)
    manifest.write_text(text)
    print(len(rows), "lexer inputs", dict(sorted(collections.Counter(r["language"] for r in rows).items())))

if __name__ == "__main__":
    main()
