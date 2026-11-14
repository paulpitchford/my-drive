"""Split Rust source into code and comments, for the checks in scripts/.

Not a parser: a small lexer that knows where comments, string literals and char literals
start and end, so the checks never mistake text inside them for code.
"""

import re

RAW_STRING = re.compile(r"r(#*)\"")


def _ident(ch):
    return ch.isalnum() or ch == "_"


def lex(text):
    """Return (code, comments) for Rust source text.

    `code` is the text with every comment and every string or char literal replaced by
    spaces, newlines kept, so line and column numbers still match the input. `comments`
    maps each 0-based line number to the text of the comments on that line.
    """
    out = []
    comments = {}
    line = 0
    i = 0
    n = len(text)

    def blank(start, end):
        nonlocal line
        for ch in text[start:end]:
            if ch == "\n":
                out.append("\n")
                line += 1
            else:
                out.append(" ")

    def comment(start, end):
        nonlocal line
        for ch in text[start:end]:
            if ch == "\n":
                out.append("\n")
                line += 1
            else:
                out.append(" ")
                comments[line] = comments.get(line, "") + ch

    while i < n:
        ch = text[i]
        prev = text[i - 1] if i else ""
        if text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
            comment(i, end)
            i = end
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            comment(i, j)
            i = j
        elif ch == "r" and (not _ident(prev) or (prev == "b" and (i < 2 or not _ident(
                text[i - 2])))) and RAW_STRING.match(text, i):
            hashes = RAW_STRING.match(text, i).group(1)
            close = '"' + hashes
            end = text.find(close, i + len(hashes) + 2)
            end = n if end < 0 else end + len(close)
            blank(i, end)
            i = end
        elif ch == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            blank(i, min(j + 1, n))
            i = j + 1
        elif ch == "'" and i + 1 < n and text[i + 1] == "\\":
            j = i + 2
            while j < n and text[j] != "'":
                j += 2 if text[j] == "\\" else 1
            blank(i, min(j + 1, n))
            i = j + 1
        elif ch == "'" and i + 2 < n and text[i + 2] == "'":
            blank(i, i + 3)
            i += 3
        else:
            out.append(ch)
            if ch == "\n":
                line += 1
            i += 1
    return "".join(out), comments
