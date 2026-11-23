"""The port logs, upstream/<app>-log.csv (issue #9, upstream/PORTING.md).

One row per upstream commit a port handled: what it was, what was decided
and what a human changed in review. Written by the port at the end of each
of its runs, for the decisions made final in that run, so a log can be
partial while its port is under way; this checks its format, so that a
malformed row fails `make check` on the port's own branch. Run with
`make test-scripts`.
"""

from __future__ import annotations

import csv
import importlib.util
import re
import sys
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
REPO = SCRIPTS.parent
HEADER = ["sha", "pr", "class", "decision", "unarr_commits", "tests", "issue", "reason", "review"]
DECISIONS = {"ported", "not applicable", "deferred"}
ALREADY_HAD_IT = "already had it:"  # behaviour Unarr had before the upstream commit (PORTING.md, section 4)


def load_triage():
    spec = importlib.util.spec_from_file_location("upstream_triage", SCRIPTS / "upstream-triage.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


CLASSES = set(load_triage().CLASSES)


def problems(text: str) -> list[str]:
    """What is wrong with a log, one line per problem; empty when nothing."""
    rows = list(csv.reader(text.splitlines()))
    if not rows or rows[0] != HEADER:
        return [f"the header is not {','.join(HEADER)}"]
    out, seen = [], set()
    for n, row in enumerate(rows[1:], 2):
        if len(row) != len(HEADER):
            out.append(f"line {n}: {len(row)} columns, not {len(HEADER)}")
            continue
        r = dict(zip(HEADER, row))
        if not re.fullmatch(r"[0-9a-f]{40}", r["sha"]):
            out.append(f"line {n}: sha is not 40 hex digits")
        elif r["sha"] in seen:
            out.append(f"line {n}: {r['sha']} logged twice")
        seen.add(r["sha"])
        if not re.fullmatch(r"#\d+", r["pr"]):
            out.append(f"line {n}: pr is not #<n>")
        if r["class"] not in CLASSES:
            out.append(f"line {n}: class {r['class']!r} is not one of the triage's")
        if r["decision"] not in DECISIONS:
            out.append(f"line {n}: decision {r['decision']!r} is not one of {sorted(DECISIONS)}")
        if any(not re.fullmatch(r"[0-9a-f]{12}", c) for c in r["unarr_commits"].split()):
            out.append(f"line {n}: unarr_commits are not 12-digit commits")
        if r["decision"] == "deferred" and not re.fullmatch(r"#\d+", r["issue"]):
            out.append(f"line {n}: a deferred commit needs its issue (#<n>)")
        if r["decision"] != "deferred" and r["issue"]:
            out.append(f"line {n}: an issue on a commit that is not deferred")
        if r["decision"] in ("not applicable", "deferred") and not r["reason"].strip():
            out.append(f"line {n}: {r['decision']} needs a reason")
        if r["reason"].startswith(ALREADY_HAD_IT):
            if r["decision"] != "ported":
                out.append(f"line {n}: {ALREADY_HAD_IT!r} is a reason of a ported commit only")
            elif not r["tests"].strip():
                out.append(f"line {n}: {ALREADY_HAD_IT!r} needs the test that covers it")
    return out


class TheLogs(unittest.TestCase):
    def test_the_shipped_logs_are_well_formed(self):
        for app in ("sonarr", "radarr"):
            with self.subTest(app=app):
                self.assertEqual(problems((REPO / "upstream" / f"{app}-log.csv").read_text(encoding="utf-8")), [])

    def test_rows_that_break_the_format(self):
        head = ",".join(HEADER) + "\n"
        sha = "a" * 40
        good = head + f'{sha},#30,schema,ported,0123456789ab,"importer::reads_243",,,\n'
        self.assertEqual(problems(good), [])
        deferred = head + f'{sha},#30,routes,deferred,0123456789ab,,#31,"over the threshold, a new subsystem",\n'
        self.assertEqual(problems(deferred), [])
        already = head + f'{sha},#30,routes,ported,,a_reset_api_key_takes_effect_at_once,,"already had it: read at each request",\n'
        self.assertEqual(problems(already), [])
        for bad, says in (
            ("sha,decision\n", "header"),
            (head + f"{sha[:12]},#30,schema,ported,,,,,\n", "40 hex"),
            (head + f"{sha},#30,schema,ported,,,,,\n{sha},#30,schema,ported,,,,,\n", "twice"),
            (head + f"{sha},30,schema,ported,,,,,\n", "pr"),
            (head + f"{sha},#30,frontend,ported,,,,,\n", "class"),
            (head + f"{sha},#30,schema,skipped,,,,,\n", "decision"),
            (head + f"{sha},#30,schema,ported,0123,,,,\n", "12-digit"),
            (head + f"{sha},#30,schema,deferred,,,,too big,\n", "needs its issue"),
            (head + f"{sha},#30,schema,ported,,,#31,,\n", "not deferred"),
            (head + f"{sha},#30,none,not applicable,,,,,\n", "needs a reason"),
            (head + f"{sha},#30,none\n", "columns"),
            (head + f"{sha},#30,routes,ported,,,,already had it: read at each request,\n", "needs the test"),
            (head + f"{sha},#30,routes,not applicable,,,,already had it: read at each request,\n", "ported commit only"),
        ):
            with self.subTest(says=says):
                found = problems(bad)
                self.assertTrue(found and says in found[0], found)

    def test_a_port_under_way_logs_only_what_is_decided(self):
        # Rows are written at the end of each run, for the final decisions only:
        # a log with some of the triage's commits, the others still to do, is well formed.
        head = ",".join(HEADER) + "\n"
        first, third = "a" * 40, "c" * 40
        partial = head + f"{first},#30,none,not applicable,,,,a project file,\n{third},#30,backend,ported,0123456789ab,plex::refresh,,,\n"
        self.assertEqual(problems(partial), [])
        self.assertEqual(problems(head), [])


if __name__ == "__main__":
    unittest.main()
