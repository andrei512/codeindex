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


def measure(grammar, data):
    root = get_parser(grammar).parse(data).root_node
    if root.has_error:
        return 0, [], []
    declarations = UNITS[grammar]
    rules = (
        ["unit lines"] if grammar == "proto" else ["branch complexity", "unit lines"]
    )
    pending = [root]
    count = 0
    labels = []
    while pending:
        node = pending.pop()
        pending.extend(reversed(node.named_children))
        if node.type not in declarations:
            continue
        trees = [node]
        if grammar == "dart":
            body = node.next_named_sibling
            if not body or body.type != "function_body":
                continue
            trees.append(body)
        count += 1
        decisions = 1
        lines = set()
        while trees:
            token = trees.pop()
            if token != node and token.type in declarations or "comment" in token.type:
                continue
            decisions += token.type.lower() in DECISIONS[grammar]
            if token.child_count:
                trees.extend(token.children)
            elif token.end_byte > token.start_byte:
                lines.update(
                    range(
                        token.start_point.row,
                        token.end_point.row + int(token.end_point.column > 0),
                    )
                )
        name = node.child_by_field_name("name")
        for rule, value, threshold in [
            ("branch complexity", decisions, 9),
            ("unit lines", len(lines), 70),
        ]:
            if rule in rules and value >= threshold:
                labels.append(
                    dict(
                        rule=rule,
                        line=node.start_point.row + 1,
                        value=value,
                        unit=name.text.decode() if name else node.type,
                    )
                )
    return count, labels, rules
