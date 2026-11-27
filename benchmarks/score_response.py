"""Read the public cx --score --json file rows used by development evaluators."""


def result_paths(answer, expected=None, *, allow_missing=False):
    files = {row["path"]: row for row in answer["files"]}
    if len(files) != len(answer["files"]):
        raise ValueError("duplicate scored path in cx response")
    if expected is not None:
        check_path_membership(files, expected, allow_missing)
    return files


def check_path_membership(files, expected, allow_missing):
    paths = set(expected)
    if len(paths) != len(expected):
        raise ValueError("duplicate expected score path")
    if extra := files.keys() - paths:
        raise ValueError(f"unexpected scored paths: {sorted(extra)}")
    if not allow_missing and (missing := paths - files.keys()):
        raise ValueError(f"missing scored paths: {sorted(missing)}")


def valid_score(score):
    return score is None or (type(score) in (float, int) and 1 <= score <= 10)


def scored_paths(answer, expected=None, *, allow_missing=False):
    files = result_paths(answer, expected, allow_missing=allow_missing)
    for row in files.values():
        score = row["score"]
        if not valid_score(score):
            raise ValueError(f"invalid score for {row['path']}: {score!r}")
    return files


def physical_lines(source):
    newline = b"\n" if isinstance(source, bytes) else "\n"
    return source.count(newline) + int(bool(source) and not source.endswith(newline))


def finding_line(finding, lines):
    line = finding["line"]
    if type(line) is not int or not 1 <= line <= lines:
        raise ValueError(f"finding requires an in-bounds integer source line: {finding}")
    return line


def included_findings(findings, rules, include_file):
    for finding in findings:
        if rules is not None and finding["rule"] not in rules:
            continue
        if include_file or finding["unit"] != "<file>":
            yield finding


def observed_findings(findings, lines, rules=None, *, include_file=True):
    """Yield findings in response order after checking their source anchors."""
    for finding in included_findings(findings, rules, include_file):
        unit = finding["unit"]
        if not isinstance(unit, str) or not unit.strip():
            raise ValueError(f"expected finding owner: {finding}")
        finding_line(finding, lines)
        yield finding
