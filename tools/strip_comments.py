#!/usr/bin/env python3
import re
import sys
from pathlib import Path

REGEX_PREFIX_CHARS = set("(,=:[!&|?{};+-*%<>~^")
REGEX_PREFIX_WORDS = {"return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "throw", "case", "do", "else"}


def _prev_significant(out, i):
    j = i - 1
    while j >= 0 and out[j] in " \t":
        j -= 1
    return j


def _regex_allowed(out):
    j = _prev_significant(out, len(out))
    if j < 0:
        return True
    c = out[j]
    if c in REGEX_PREFIX_CHARS or c == "\n":
        return True
    if c.isalnum() or c == "_" or c == "$":
        k = j
        while k >= 0 and (out[k].isalnum() or out[k] in "_$"):
            k -= 1
        return out[k + 1:j + 1] in REGEX_PREFIX_WORDS
    return False


def strip_js(src):
    out = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = src.find("\n", i)
            i = n if j < 0 else j
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            j = src.find("*/", i + 2)
            i = n if j < 0 else j + 2
            continue
        if c in "'\"":
            j = i + 1
            while j < n and src[j] != c:
                if src[j] == "\\":
                    j += 1
                if src[j] == "\n":
                    break
                j += 1
            out.append(src[i:j + 1])
            i = j + 1
            continue
        if c == "`":
            i = _template(src, i, out)
            continue
        if c == "/" and _regex_allowed("".join(out)):
            j = i + 1
            in_class = False
            while j < n and src[j] != "\n":
                if src[j] == "\\":
                    j += 1
                elif src[j] == "[":
                    in_class = True
                elif src[j] == "]":
                    in_class = False
                elif src[j] == "/" and not in_class:
                    break
                j += 1
            while j + 1 < n and src[j + 1].isalpha():
                j += 1
            out.append(src[i:j + 1])
            i = j + 1
            continue
        out.append(c)
        i += 1
    return _drop_blank_comment_lines(src, "".join(out))


def _template(src, i, out):
    n = len(src)
    out.append("`")
    j = i + 1
    while j < n:
        c = src[j]
        if c == "\\":
            out.append(src[j:j + 2])
            j += 2
            continue
        if c == "`":
            out.append("`")
            return j + 1
        if c == "$" and j + 1 < n and src[j + 1] == "{":
            out.append("${")
            j += 2
            depth = 1
            inner = []
            while j < n and depth > 0:
                d = src[j]
                if d == "{":
                    depth += 1
                elif d == "}":
                    depth -= 1
                    if depth == 0:
                        break
                inner.append(d)
                j += 1
            out.append(strip_js("".join(inner)))
            out.append("}")
            j += 1
            continue
        out.append(c)
        j += 1
    return j


def strip_css(src):
    out = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            j = src.find("*/", i + 2)
            i = n if j < 0 else j + 2
            continue
        if c in "'\"":
            j = i + 1
            while j < n and src[j] != c:
                if src[j] == "\\":
                    j += 1
                j += 1
            out.append(src[i:j + 1])
            i = j + 1
            continue
        out.append(c)
        i += 1
    return _drop_blank_comment_lines(src, "".join(out))


def _drop_blank_comment_lines(before, after):
    """Remove lines that became empty because they held only a comment."""
    before_lines = before.split("\n")
    after_lines = after.split("\n")
    if len(before_lines) != len(after_lines):
        return after
    kept = [a for b, a in zip(before_lines, after_lines) if a.strip() or not b.strip()]
    return "\n".join(kept)


_HTML_COMMENT_LINE = re.compile(r"^[ \t]*<!--.*?-->[ \t]*\n", re.S | re.M)
_HTML_COMMENT = re.compile(r"<!--.*?-->", re.S)
_BLOCK = re.compile(r"(<(script|style)\b[^>]*>)(.*?)(</\2\s*>)", re.S | re.I)


def strip_html(src):
    src = _HTML_COMMENT.sub("", _HTML_COMMENT_LINE.sub("", src))

    def repl(m):
        body = m.group(3)
        body = strip_js(body) if m.group(2).lower() == "script" else strip_css(body)
        return m.group(1) + body + m.group(4)

    return _BLOCK.sub(repl, src)


HANDLERS = {".js": strip_js, ".mjs": strip_js, ".css": strip_css, ".html": strip_html, ".htm": strip_html}


def main(paths):
    for p in map(Path, paths):
        handler = HANDLERS.get(p.suffix.lower())
        if handler is None:
            print(f"strip_comments: skipping {p} (unknown type)", file=sys.stderr)
            continue
        text = p.read_text(encoding="utf-8")
        p.write_text(handler(text), encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main(sys.argv[1:])
