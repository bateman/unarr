#!/usr/bin/env python3
"""Extract NUnit `[TestCase(...)]` data from the originals' test sources as JSON.

The Sonarr and Radarr repositories' parser and decision-engine tests are the
specification Unarr's ports are checked against. This script reads the C#
fixture files and writes, for every test method, the list of its test cases
with the arguments decoded, so the Rust tests can replay them as data::

    scripts/port-vectors.py --sonarr /path/to/Sonarr --out tests/vectors
    scripts/port-vectors.py --source /path/to/Radarr --out tests/vectors/radarr

Output: `<out>/<area>/<Fixture>.json` holding
``{"source": "...cs", "methods": {"should_x": {"params": [...], "cases": [[...], ...]}}}``.
Argument literals understood: strings (regular, verbatim `@""`, with escapes),
integers, decimals, booleans, `null`, `new[] { ... }` and `new int[] { ... }`
arrays, and `Enum.Member` references, kept as `{"enum": "Enum", "member": "Member"}`.
Methods whose cases come from a static `object[]` field (`TestCaseSource`) are
read too when the field is in the same file.

Sonarr and Radarr are GPL-3.0, as Unarr is; the vectors keep their origin in `source`.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ATTR_RE = re.compile(r"\[TestCase(?:\((?P<args>.*?)\))?\]\s*$", re.S)


class Lexer:
    """A small tokenizer for C# attribute argument lists."""

    def __init__(self, text: str):
        self.text = text
        self.pos = 0

    def skip_ws(self):
        while self.pos < len(self.text) and self.text[self.pos] in " \t\r\n":
            self.pos += 1

    def peek(self) -> str:
        self.skip_ws()
        return self.text[self.pos] if self.pos < len(self.text) else ""

    def parse_value(self):
        c = self.peek()
        if c == '"':
            return self.parse_string(verbatim=False)
        if c == "@" and self.text[self.pos + 1] == '"':
            self.pos += 1
            return self.parse_string(verbatim=True)
        if self.text.startswith("new", self.pos):
            return self.parse_array()
        if c in "-0123456789.":
            return self.parse_number()
        ident = self.parse_ident()
        if ident == "true":
            return True
        if ident == "false":
            return False
        if ident == "null":
            return None
        if ident.startswith("nameof"):
            raise ValueError(ident)
        if "." in ident:
            enum, _, member = ident.rpartition(".")
            return {"enum": enum, "member": member}
        return {"ident": ident}

    def parse_string(self, verbatim: bool) -> str:
        assert self.text[self.pos] == '"'
        self.pos += 1
        out = []
        while True:
            c = self.text[self.pos]
            if verbatim:
                if c == '"':
                    if self.text[self.pos + 1 : self.pos + 2] == '"':
                        out.append('"')
                        self.pos += 2
                        continue
                    self.pos += 1
                    break
                out.append(c)
                self.pos += 1
            else:
                if c == "\\":
                    nxt = self.text[self.pos + 1]
                    simple = {"n": "\n", "t": "\t", "r": "\r", '"': '"', "\\": "\\", "0": "\0", "'": "'", "b": "\b"}
                    if nxt in simple:
                        out.append(simple[nxt])
                        self.pos += 2
                    elif nxt == "u":
                        out.append(chr(int(self.text[self.pos + 2 : self.pos + 6], 16)))
                        self.pos += 6
                    else:
                        raise ValueError(f"escape \\{nxt}")
                    continue
                if c == '"':
                    self.pos += 1
                    break
                out.append(c)
                self.pos += 1
        s = "".join(out)
        # Extension calls such as `.AsOsAgnostic()` are test plumbing.
        while True:
            call = re.match(r"\s*\.\w+\([^)]*\)", self.text[self.pos :])
            if not call:
                break
            self.pos += call.end()
        # Adjacent string concatenation: "a" + "b".
        if self.peek() == "+":
            self.pos += 1
            s += self.parse_value()
        return s

    def parse_number(self):
        m = re.match(r"-?\d+(\.\d+)?[mMdDfF]?", self.text[self.pos :])
        if not m:
            raise ValueError(self.text[self.pos : self.pos + 20])
        self.pos += m.end()
        tok = m.group(0).rstrip("mMdDfF")
        value = float(tok) if "." in tok else int(tok)
        # Constant arithmetic such as `50 * 6`.
        product = re.match(r"\s*\*\s*(-?\d+)", self.text[self.pos :])
        if product:
            self.pos += product.end()
            value *= int(product.group(1))
        return value

    def parse_ident(self) -> str:
        m = re.match(r"[A-Za-z_][\w.]*(?:\([^)]*\))?", self.text[self.pos :])
        if not m:
            raise ValueError(self.text[self.pos : self.pos + 20])
        self.pos += m.end()
        return m.group(0)

    def parse_array(self) -> list:
        empty = re.match(r"new\s*[\w<>]+\s*\[\s*0\s*\]", self.text[self.pos :])
        if empty:
            self.pos += empty.end()
            return []
        m = re.match(r"new\s*(?:[\w<>\[\]]+)?\s*\[\s*\]\s*\{", self.text[self.pos :]) or re.match(
            r"new\s*(?:[\w<>]+)?\s*\{", self.text[self.pos :]
        )
        if not m:
            raise ValueError(self.text[self.pos : self.pos + 30])
        self.pos += m.end()
        items = []
        while True:
            if self.peek() == "}":
                self.pos += 1
                break
            items.append(self.parse_value())
            if self.peek() == ",":
                self.pos += 1
        return items

    def parse_args(self) -> list:
        args = []
        while self.peek():
            if self.peek() == ",":
                self.pos += 1
                continue
            # Named arguments such as `Description = "..."` are NUnit metadata.
            named = re.match(r"\s*(\w+)\s*=(?!=)", self.text[self.pos :])
            if named:
                self.pos += named.end()
                self.parse_value()
                continue
            args.append(self.parse_value())
        return args


def split_attributes(block: str) -> list[str]:
    """`[TestCase(...)]` attributes in a block, each as its argument text."""
    out = []
    i = 0
    while True:
        i = block.find("[TestCase", i)
        if i < 0:
            return out
        j = i + len("[TestCase")
        if block[j] == "]":
            out.append("")
            i = j
            continue
        if block[j] != "(":  # [TestCaseSource(...)], handled separately
            i = j
            continue
        depth, k, in_str, verbatim = 0, j, False, False
        while True:
            c = block[k]
            if in_str:
                if c == "\\" and not verbatim:
                    k += 2
                    continue
                if c == '"':
                    if verbatim and block[k + 1 : k + 2] == '"':
                        k += 2
                        continue
                    in_str = False
            elif c == '"':
                in_str = True
                verbatim = block[k - 1] == "@"
            elif c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        out.append(block[j + 1 : k])
        i = k


METHOD_RE = re.compile(r"public\s+void\s+(?P<name>\w+)\s*\((?P<params>[^)]*)\)", re.S)
STATIC_SOURCE_RE = re.compile(r"public\s+static\s+object\[\]\s+(?P<name>\w+)\s*=\s*\{(?P<body>.*?)\n\s*\};", re.S)
SOURCE_ATTR_RE = re.compile(r"\[Test,\s*TestCaseSource\(\s*(?:nameof\()?\"?(?P<field>\w+)\"?\)?\s*\)\]|\[TestCaseSource\(\s*(?:nameof\()?\"?(?P<field2>\w+)\"?\)?\s*\)\]")


def extract(path: Path) -> dict:
    text = path.read_text(encoding="utf-8-sig")
    # Strip line comments so commented-out cases are not read.
    text = re.sub(r"^\s*//.*$", "", text, flags=re.M)
    sources = {m.group("name"): m.group("body") for m in STATIC_SOURCE_RE.finditer(text)}
    methods: dict[str, dict] = {}
    pos = 0
    for m in METHOD_RE.finditer(text):
        block = text[pos : m.start()]
        pos = m.end()
        params = [p.strip().split()[-1] for p in m.group("params").split(",") if p.strip()]
        cases = []
        for args in split_attributes(block):
            try:
                cases.append(Lexer(args).parse_args() if args else [])
            except (ValueError, IndexError, AssertionError) as e:
                print(f"[vectors] {path.name}: cannot decode TestCase({args[:60]}...): {e}", file=sys.stderr)
        for sm in SOURCE_ATTR_RE.finditer(block):
            field = sm.group("field") or sm.group("field2")
            body = sources.get(field)
            if body is None:
                print(f"[vectors] {path.name}: TestCaseSource {field} not found in file", file=sys.stderr)
                continue
            rows = re.findall(r"new object\[\]\s*\{(.*?)\}\s*,?", body, flags=re.S)
            for row in rows:
                try:
                    cases.append(Lexer(row).parse_args())
                except (ValueError, IndexError) as e:
                    print(f"[vectors] {path.name}: cannot decode source row {row[:60]}: {e}", file=sys.stderr)
        if cases:
            methods[m.group("name")] = {"params": params, "cases": cases}
    return {"source": str(path).split("/src/", 1)[-1], "methods": methods}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument(
        "--source",
        "--sonarr",
        dest="source",
        required=True,
        type=Path,
        help="checkout of github.com/Sonarr/Sonarr or github.com/Radarr/Radarr",
    )
    ap.add_argument("--out", default="tests/vectors", type=Path)
    ap.add_argument("--area", action="append", default=None, help="test directory under src/NzbDrone.Core.Test (default: ParserTests, DecisionEngineTests)")
    args = ap.parse_args()
    areas = args.area or ["ParserTests", "DecisionEngineTests"]
    total = 0
    for area in areas:
        src = args.source / "src" / "NzbDrone.Core.Test" / area
        out_dir = args.out / area.removesuffix("Tests").lower()
        out_dir.mkdir(parents=True, exist_ok=True)
        for path in sorted(src.rglob("*Fixture.cs")) + sorted(src.rglob("*Regex.cs")):
            data = extract(path)
            if not data["methods"]:
                continue
            n = sum(len(m["cases"]) for m in data["methods"].values())
            total += n
            (out_dir / (path.stem + ".json")).write_text(json.dumps(data, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
            print(f"[vectors] {area}/{path.name}: {len(data['methods'])} methods, {n} cases", file=sys.stderr)
    print(f"[vectors] {total} cases written under {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
