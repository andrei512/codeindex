"""Advance one-based UTF-16 fixture coordinates across literal source text."""


def utf16_width(text):
    return len(text) if text.isascii() else len(text.encode("utf-16-le")) // 2


def advance(line, column, text):
    if "\n" in text:
        line += text.count("\n")
        text = text.rsplit("\n", 1)[-1]
        column = 1
    return line, column + utf16_width(text)


def utf16_prefix(text, column):
    """Convert a positive UTF-16 cursor to the Unicode prefix at that boundary."""
    encoded = text.encode("utf-16-le")
    stop = (column - 1) * 2
    if not 0 <= stop <= len(encoded):
        raise ValueError(f"UTF-16 column outside source line: {column}")
    return encoded[:stop].decode("utf-16-le")


class SourceLines:
    """Physical source lines for a single immutable fixture version."""
    def __init__(self, data):
        self.lines = data.split(b"\n")

    def at(self, line):
        if not 1 <= line <= len(self.lines):
            raise ValueError(f"line outside source: {line}")
        return self.lines[line - 1].removesuffix(b"\r").decode()
