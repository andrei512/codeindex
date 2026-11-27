"""Independent development labels from pinned grammars for profiles outside Lizard."""

from tree_sitter_language_pack import get_parser

# Each language specifies the unit and decision nodes that are measured.
UNITS = {
    "bash": {"function_definition"},
    "powershell": {"function_statement"},
    "starlark": {"function_definition"},
    "cmake": {"function_def", "macro_def"},
    "hack": {"function_declaration", "method_declaration"},
    "dart": {"function_signature", "method_signature"},
    "proto": {"message"},
}
DECISIONS = {
    "bash": {
        "if_statement",
        "elif_clause",
        "for_statement",
        "while_statement",
        "case_item",
        "&&",
        "||",
    },
    "powershell": {
        "if_statement",
        "elseif_clause",
        "foreach_statement",
        "for_statement",
        "while_statement",
        "catch_clause",
        "-and",
        "-or",
    },
    "starlark": {"if_statement", "elif_clause", "for_statement", "and", "or"},
    "cmake": {"if_command", "elseif_command", "foreach_command", "while_command"},
    "hack": {
        "if_statement",
        "elseif_clause",
        "for_statement",
        "foreach_statement",
        "while_statement",
        "catch_clause",
        "&&",
        "||",
    },
    "dart": {
        "if_statement",
        "for_statement",
        "while_statement",
        "catch_clause",
        "&&",
        "||",
    },
    "proto": set(),
}


def declaration_nodes(root, declarations):
    pending = [root]
    while pending:
        node = pending.pop()
        pending.extend(reversed(node.named_children))
        if node.type in declarations:
            yield node


def unit_roots(node, grammar):
    if grammar != "dart":
        return [node]
    body = node.next_named_sibling
    return [node, body] if body and body.type == "function_body" else []


def physical_token_lines(token):
    return range(token.start_point.row, token.end_point.row + int(token.end_point.column > 0))


def measure_unit(node, trees, declarations, decisions):
    total = 1
    lines = set()
    while trees:
        token = trees.pop()
        if "comment" in token.type:
            continue
        if token != node and token.type in declarations:
            continue
        total += token.type.lower() in decisions
        measure_token(token, trees, lines)
    return total, len(lines)


def measure_token(token, trees, lines):
    if token.child_count:
        trees.extend(token.children)
    elif token.end_byte > token.start_byte:
        lines.update(physical_token_lines(token))


def unit_findings(node, rules, decisions, lines):
    name = node.child_by_field_name("name")
    for rule, value, threshold in [
        ("branch complexity", decisions, 9),
        ("unit lines", lines, 70),
    ]:
        if rule in rules and value >= threshold:
            yield dict(
                rule=rule,
                line=node.start_point.row + 1,
                value=value,
                unit=name.text.decode() if name else node.type,
            )


def measure(grammar, data):
    root = get_parser(grammar).parse(data).root_node
    if root.has_error:
        return 0, [], []
    declarations = UNITS[grammar]
    rules = (
        ["unit lines"] if grammar == "proto" else ["branch complexity", "unit lines"]
    )
    count = 0
    labels = []
    for node in declaration_nodes(root, declarations):
        trees = unit_roots(node, grammar)
        if not trees:
            continue
        count += 1
        decisions, lines = measure_unit(node, trees, declarations, DECISIONS[grammar])
        labels.extend(unit_findings(node, rules, decisions, lines))
    return count, labels, rules
