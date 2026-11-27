"""Read source-spelled routine definitions and direct calls from a Clang translation unit."""
from pathlib import Path
import re
from clang import cindex
from language_fixture_positions import utf16_width


def whole_spelling_at(source, spelling, offset):
    if not source.startswith(spelling, offset):
        return False
    end = offset + len(spelling)
    return not (re.match(rb"\w", source[max(0, offset - 1):offset])
                or re.match(rb"\w", source[end:end + 1]))


def source_sites(unit, location, source, path):
    """Accept a token only when its spelling matches literal bytes at Clang's offset."""
    if not location.file or Path(str(location.file)) != path:
        return None
    name = unit.spelling
    if not re.fullmatch(r"[A-Za-z_]\w*", name):
        return None
    offset = location.offset
    if not whole_spelling_at(source, name.encode(), offset):
        return None
    start = source.rfind(b"\n", 0, offset) + 1
    col = utf16_width(source[start:offset].decode()) + 1
    return f"{location.line}:{col}"


class SourceBindings:
    """Cache the definition of each referenced compiler cursor inside one translation unit."""
    def __init__(self, path):
        self.path = path
        self.source = path.read_bytes()
        self.routines = {cindex.CursorKind.FUNCTION_DECL, cindex.CursorKind.CXX_METHOD}
        self.targets = {}

    def nodes(self, root):
        """Only descend into the measured file; included declarations are out of scope."""
        stack = list(root.get_children())
        while stack:
            node = stack.pop()
            if node.location.file and Path(str(node.location.file)) == self.path:
                children = list(node.get_children())
                stack.extend(children)
                yield node, children

    def site(self, node):
        return source_sites(node, node.location, self.source, self.path)

    def definition(self, target):
        if target not in self.targets:
            decl = target.get_definition()
            self.targets[target] = self.site(decl) if decl and target.location == decl.location else None
        return self.targets[target]

    def calls(self, node, callee):
        target = node.referenced
        if not target or target.kind not in self.routines:
            return
        decl = self.definition(target)
        if not decl:
            return
        # Call arguments are independent expressions, never callee evidence.
        ref_kinds = (cindex.CursorKind.DECL_REF_EXPR, cindex.CursorKind.MEMBER_REF_EXPR)
        for expr in callee.walk_preorder():
            if expr.kind in ref_kinds and expr.referenced == target:
                site = self.site(expr)
                if site:
                    yield expr.spelling, site, decl


def translation_unit(path, flags):
    tu = cindex.Index.create().parse(str(path), args=flags)
    errors = [str(d) for d in tu.diagnostics if d.severity >= cindex.Diagnostic.Error]
    if errors:
        raise RuntimeError("\n".join(errors))
    return tu


def collect(path, flags):
    tu = translation_unit(path, flags)
    k = cindex.CursorKind
    bindings = SourceBindings(path)
    defs, calls = set(), set()
    for node, children in bindings.nodes(tu.cursor):
        if node.kind in bindings.routines and node.is_definition():
            target = bindings.site(node)
            if target:
                defs.add((node.spelling, target))
        if node.kind != k.CALL_EXPR or not children:
            continue
        calls.update(bindings.calls(node, children[0]))
    return sorted(defs), sorted(calls)
