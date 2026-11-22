#!/usr/bin/env python3
"""Bring a personality to a new upstream release and say what changed.

The deterministic step between the triage and the port (issue #9, section
2): given the target release, it writes the new pin, records again from the
new original what is tied to the pin, and writes a summary that is the
starting material of the port::

    scripts/pin-bump.py --app radarr --dry-run
    scripts/pin-bump.py --app radarr --target 7.0.3.10918 --releases tags
    scripts/pin-bump.py --app radarr --steps fetch,record      # the job without secrets
    scripts/pin-bump.py --app radarr --steps apply,test,summary --work DIR

The steps, in order. ``--steps`` runs a subset; each reads what the earlier
ones left in the work directory, ``<UNARR_CACHE or .cache>/bump/<app>``
(``--work``), so they can run in different jobs:

``fetch``
    The target release from the triage (``scripts/upstream-triage.py``, the
    rule of the issue) or ``--target``; its tarball downloaded from the
    project's GitHub release URL into a directory of its own, the final
    host checked to be GitHub's release storage, the archive's entries
    checked, its SHA-256 computed and compared with the digest the GitHub
    API publishes when the releases were read from the API. Writes only the
    work directory: ``bump.json``, ``triage.json``, ``triage.md``.
``record``
    Runs the new original, as ``scripts/<app>-fixture.sh golden`` does, on
    copies of the fixture: first with no request, to read the migration
    level of the fixture it migrates (and keep that database); then every
    request list under ``tests/fixtures/<app>/golden*/``, with the variables
    its header names; the provider schemas and the localisation file with
    the main list; the files the lists' headers say were copied by hand
    (``db-after.json``, ``config-after.xml``); and what the mock was asked.
    Writes only the work directory (and the fixture script's ``/movies``,
    ``/tv``, ``/downloads``). It is the only step that runs a binary nobody
    vetted: it can run in a job of its own, without secrets, which hands
    the work directory on as an artifact. ``--baseline`` also records the
    pinned release the same way, to tell what any recording changes from
    what the new release changes.
``apply``
    Writes the repository: ``upstream/<app>.pin`` (version, build time,
    migration, SHA-256) and the cursor ``upstream/<app>.lock`` (not with
    ``--keep-cursor``); the recordings, after comparing them with the
    committed ones; the old binary removed from the cache and ``make
    ui-<app>`` run, which downloads the tarball again and checks it against
    the new pin; the test vectors of the whole new tag
    (``scripts/port-vectors.py``), in place of the old ones; the importer's
    migration range checked. It never runs the original and reads the work
    directory as untrusted: only the files a request list names are copied,
    and each is checked first.
``test``
    ``cargo test --workspace --no-fail-fast`` and the scripts' tests; each
    failure run once more (``--rerun``), to tell a flaky test from a broken
    one.
``summary``
    ``summary.md`` and ``summary.json`` in the work directory: the release,
    which recordings changed and how, the vectors, the importer, which
    tests fail, and what is left to check by hand.

The importer's ``MAX_KNOWN_MIGRATION`` is not raised here: the test that
compares it with the pin (``the_pinned_release_is_within_the_known_migrations``)
is the reminder that the mapping must be checked against the new
migrations first. The summary says where it is and which migrations to read.

Upstream text (commit subjects) and what the new original answered are
untrusted data: printed in code spans, never acted on.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import time
import urllib.parse
from dataclasses import dataclass, field
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO = SCRIPTS.parent
UPSTREAM = REPO / "upstream"

_spec = importlib.util.spec_from_file_location("upstream_triage", SCRIPTS / "upstream-triage.py")
triage = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = triage
_spec.loader.exec_module(triage)

STEPS = ("fetch", "record", "apply", "test", "summary")


@dataclass(frozen=True)
class App:
    key: str
    name: str
    # The release asset, as the fixture script downloads it; {branch} is the
    # cursor's branch (main, master), as in the asset names.
    asset: str
    db: str
    # Variable names the fixture script reads.
    version_env: str
    sha_env: str
    vectors: str
    importer: str
    schemas: str
    # The fixture script's library directory variable, for the summary.
    media_env: str

    @property
    def fixture_script(self) -> Path:
        return SCRIPTS / f"{self.key}-fixture.sh"

    @property
    def fixtures(self) -> Path:
        return REPO / "tests" / "fixtures" / self.key


APPS = {
    "sonarr": App("sonarr", "Sonarr", "Sonarr.{branch}.{version}.linux-x64.tar.gz", "sonarr.db",
                  "SONARR_VERSION", "SONARR_SHA256", "tests/vectors", "crates/unarr-import/src/sonarr.rs",
                  "crates/unarr-api/schemas/sonarr", "TV_DIR"),
    "radarr": App("radarr", "Radarr", "Radarr.{branch}.{version}.linux-core-x64.tar.gz", "radarr.db",
                  "RADARR_VERSION", "RADARR_SHA256", "tests/vectors/radarr", "crates/unarr-import/src/radarr.rs",
                  "crates/unarr-api/schemas/radarr", "MOVIES_DIR"),
}

# The directories of the vectors, by the upstream test directory they come from.
VECTOR_AREAS = {"ParserTests": "parser", "DecisionEngineTests": "decisionengine"}

# Where GitHub serves release assets from, after the redirect of
# github.com/<owner>/<repo>/releases/download/...
ASSET_HOSTS = {"release-assets.githubusercontent.com", "objects.githubusercontent.com",
               "github-releases.githubusercontent.com"}

# What a request list's header may set (see tests/fixtures/<app>/golden-*/requests.txt).
HEADER_VARIABLES = {"GOLDEN_SETUP", "GOLDEN_REQUESTS", "GOLDEN_OUT", "GOLDEN_SIGNALR"}
# Files a header says were copied by hand after the recording, and how
# the record step makes them.
HAND_COPIED = ("db-after.json", "config-after.xml")

MAX_FILE = 64 * 1024 * 1024
SHA256 = re.compile(r"^[0-9a-f]{64}$")
BUILD_TIME = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
KNOWN_FLAKY = {
    "golden_responses_match": "Sonarr's goldens (crates/unarr-api/tests/contract.rs), the import queue "
                              "sequence: failed once in CI and passed when run again on the same commit",
}


class BumpError(SystemExit):
    pass


def die(msg: str) -> None:
    raise BumpError(f"[bump] {msg}")


def log(msg: str) -> None:
    print(f"[bump] {msg}", file=sys.stderr, flush=True)


def cache_dir() -> Path:
    """UNARR_CACHE holds what .cache holds, as for the triage."""
    cache = os.environ.get("UNARR_CACHE")
    return Path(cache) if cache else REPO / ".cache"


def fixture_cache(app: App) -> Path:
    """Where scripts/<app>-fixture.sh keeps the pinned tarball and binary:
    UNARR_CACHE itself when set (as CLAUDE.md says), .cache/<app> otherwise."""
    cache = os.environ.get("UNARR_CACHE")
    return Path(cache) if cache else REPO / ".cache" / app.key


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, data) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=1, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run(cmd: list[str], logfile: Path, env: dict | None = None, cwd: Path = REPO) -> int:
    """Runs `cmd`, its output appended to `logfile`; returns the exit code."""
    logfile.parent.mkdir(parents=True, exist_ok=True)
    with logfile.open("ab") as out:
        out.write(f"$ {' '.join(cmd)}\n".encode())
        out.flush()
        return subprocess.run(cmd, cwd=cwd, env=env, stdout=out, stderr=subprocess.STDOUT).returncode


# --- the pin and the cursor -------------------------------------------------


PIN_KEYS = ("version", "build_time", "migration", "sha256")


def check_pin_values(values: dict) -> None:
    try:
        triage.Version.parse(str(values["version"]))
    except ValueError as err:
        die(str(err))
    if not BUILD_TIME.match(str(values["build_time"])):
        die(f"not a build time YYYY-MM-DDTHH:MM:SSZ: {values['build_time']!r}")
    if not isinstance(values["migration"], int) or not 1 <= values["migration"] <= 100000:
        die(f"not a migration level: {values['migration']!r}")
    if not SHA256.match(str(values["sha256"])):
        die(f"not a SHA-256: {values['sha256']!r}")


def replace_values(text: str, values: dict, keys: tuple[str, ...], what: str) -> str:
    """`key = value` lines of a pin or a lock with new values; comments and
    order kept. Each key must be there exactly once."""
    out = text
    for key in keys:
        pattern = re.compile(rf"^({re.escape(key)}[ \t]*=[ \t]*)(.*?)[ \t]*$", re.M)
        found = pattern.findall(out)
        if len(found) != 1:
            die(f"{what}: expected one `{key} = ...` line, found {len(found)}")
        out = pattern.sub(lambda m: m.group(1) + str(values[key]), out)
    return out


def write_pin(path: Path, values: dict) -> str:
    check_pin_values(values)
    text = replace_values(path.read_text(), values, PIN_KEYS, str(path))
    path.write_text(text)
    return text


def write_lock(path: Path, release: str, commit: str) -> str:
    try:
        triage.Version.parse(release)
    except ValueError as err:
        die(str(err))
    if not triage.SHA.match(commit):
        die(f"not a full commit SHA: {commit!r}")
    text = replace_values(path.read_text(), {"release": release, "commit": commit}, ("release", "commit"), str(path))
    path.write_text(text)
    return text


def read_pin(path: Path) -> dict[str, str]:
    values = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            key, _, value = line.partition("=")
            values[key.strip()] = value.strip()
    return values


# --- the request lists ------------------------------------------------------


@dataclass
class GoldenList:
    """One directory of recordings: its request list and how it is recorded."""
    dir: Path
    env: dict[str, str] = field(default_factory=dict)
    names: list[str] = field(default_factory=list)

    @property
    def key(self) -> str:
        return self.dir.name

    def outputs(self, app: App) -> list[str]:
        """The files a recording of this list writes, as named by the list."""
        out = [f"{n}.json" for n in self.names]
        # Radarr's fixture script always listens to the hub, Sonarr's when asked.
        if app.key == "radarr" or self.env.get("GOLDEN_SIGNALR") == "1":
            out.append("signalr.json")
        out += [f for f in HAND_COPIED if (self.dir / f).exists()]
        return out


def parse_requests(text: str) -> list[str]:
    """The names a request list records, in order (record-golden.py's format):
    `sleep N` and `mock - ...` record nothing."""
    names = []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if parts[0] == "sleep":
            continue
        name = parts[1] if parts[0] == "mock" and len(parts) > 1 else parts[0]
        if name == "-":
            continue
        if not NAME.match(name):
            die(f"odd request name {name!r}")
        names.append(name)
    return names


def parse_header(text: str, list_dir: str) -> dict[str, str]:
    """The variables of the command a list's header gives to record it:
    `#   VAR=value \\` lines ending with one that runs `make golden...`.
    `list_dir` is the list's directory, relative to the repository."""
    env: dict[str, str] = {}
    lines = []
    for line in text.splitlines():
        if not line.startswith("#"):
            break
        lines.append(line[1:].strip())
    for i, line in enumerate(lines):
        if not re.match(r"^[A-Z_]+=\S+", line):
            continue
        block = []
        for follow in lines[i:]:
            block.append(follow)
            if "make golden" in follow or not follow.endswith("\\"):
                break
        if "make golden" not in block[-1]:
            continue
        for var, value in re.findall(r"(?:^|\s)([A-Z_]+)=(\S+)", " ".join(block)):
            if var not in HEADER_VARIABLES:
                die(f"{list_dir}/requests.txt: the header sets {var}, which the bump does not know how to pass on")
            env[var] = value
        break
    for var, own in (("GOLDEN_REQUESTS", f"{list_dir}/requests.txt"), ("GOLDEN_OUT", list_dir)):
        if var in env and env[var].rstrip("/") != own:
            die(f"{list_dir}/requests.txt: the header's {var}={env[var]} is not the list's own ({own})")
    if "GOLDEN_SETUP" in env and not env["GOLDEN_SETUP"].startswith(list_dir + "/"):
        die(f"{list_dir}/requests.txt: GOLDEN_SETUP {env['GOLDEN_SETUP']} is outside the list's directory")
    return env


def golden_lists(app: App, only: list[str] | None = None) -> list[GoldenList]:
    """Every request list of the personality, the main one first."""
    out = []
    dirs = sorted(d for d in app.fixtures.glob("golden*") if (d / "requests.txt").is_file())
    for d in sorted(dirs, key=lambda d: (d.name != "golden", d.name)):
        if only and d.name not in only:
            continue
        rel = d.relative_to(REPO).as_posix()
        text = (d / "requests.txt").read_text()
        env = parse_header(text, rel) if d.name != "golden" else {}
        out.append(GoldenList(d, env, parse_requests(text)))
    if only:
        missing = set(only) - {g.key for g in out}
        if missing:
            die(f"no request list {', '.join(sorted(missing))} under {app.fixtures.relative_to(REPO)}")
    return out


# --- comparing recordings ---------------------------------------------------


TIME = re.compile(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?$")
# Keys whose value is the moment of the recording, not the original's answer.
VOLATILE_KEYS = {"recordedAt"}


def normalise(value):
    """What two recordings of one answer share: instants written as `<time>`,
    whole floats as integers, covers' `?lastWrite=` cut, the recording's
    own instant dropped."""
    if isinstance(value, dict):
        return {k: normalise(v) for k, v in value.items() if k not in VOLATILE_KEYS}
    if isinstance(value, list):
        return [normalise(v) for v in value]
    if isinstance(value, float) and value.is_integer() and abs(value) < 1e15:
        return int(value)
    if isinstance(value, str):
        if TIME.match(value):
            return "<time>"
        at = value.find("?lastWrite=")
        if at >= 0:
            return value[:at] + "?lastWrite=…"
    return value


def shape(value, prefix: str = "", out: set | None = None) -> set:
    """Key paths of a JSON tree; a list's items are merged under `[]`."""
    out = set() if out is None else out
    if isinstance(value, dict):
        for k, v in value.items():
            out.add(f"{prefix}/{k}")
            shape(v, f"{prefix}/{k}", out)
    elif isinstance(value, list):
        out.add(f"{prefix}[]")
        for item in value:
            shape(item, f"{prefix}[]", out)
    return out


def short(value, limit: int = 60) -> str:
    text = json.dumps(value, ensure_ascii=False, sort_keys=True)
    return text if len(text) <= limit else text[: limit - 1] + "…"


def value_diff(old, new, path: str = "", out: list | None = None, limit: int = 8) -> list[str]:
    """The first differences between two normalised trees, as `path: old -> new`."""
    out = [] if out is None else out
    if len(out) >= limit:
        return out
    if isinstance(old, dict) and isinstance(new, dict):
        for k in old:
            if k not in new:
                out.append(f"{path}/{k}: gone")
            else:
                value_diff(old[k], new[k], f"{path}/{k}", out, limit)
            if len(out) >= limit:
                return out
        for k in new:
            if k not in old and len(out) < limit:
                out.append(f"{path}/{k}: new {short(new[k])}")
    elif isinstance(old, list) and isinstance(new, list):
        if len(old) != len(new):
            out.append(f"{path}: {len(old)} items -> {len(new)}")
        for i, (a, b) in enumerate(zip(old, new)):
            value_diff(a, b, f"{path}[{i}]", out, limit)
            if len(out) >= limit:
                return out
    elif old != new:
        out.append(f"{path}: {short(old)} -> {short(new)}")
    return out


def compare_json(old, new) -> dict:
    """How a recording changed: `same`, `time` (only instants differ),
    `status`, `shape` (keys added or gone), `values`; `new` and `gone` for
    a file only one side has."""
    if old is None and new is None:
        return {"kind": "same"}
    if old is None:
        return {"kind": "new"}
    if new is None:
        return {"kind": "gone"}
    golden = isinstance(old, dict) and isinstance(new, dict) and "status" in old and "body" in old
    if golden and old.get("status") != new.get("status"):
        return {"kind": "status", "status": [old.get("status"), new.get("status")],
                "values": value_diff(normalise(old.get("body")), normalise(new.get("body")))}
    if golden:
        a, b = old.get("body"), new.get("body")
        if a == b and old.get("request") == new.get("request"):
            return {"kind": "same"}
    else:
        a, b = old, new
        if a == b:
            return {"kind": "same"}
    na, nb = normalise(a), normalise(b)
    if na == nb:
        return {"kind": "time"}
    sa, sb = shape(na), shape(nb)
    if sa != sb:
        return {"kind": "shape", "added": sorted(sb - sa)[:12], "removed": sorted(sa - sb)[:12],
                "values": value_diff(na, nb)}
    return {"kind": "values", "values": value_diff(na, nb)}


def compare_text(old: str | None, new: str | None) -> dict:
    if old is None and new is None:
        return {"kind": "same"}
    if old is None:
        return {"kind": "new"}
    if new is None:
        return {"kind": "gone"}
    return {"kind": "same" if old == new else "values"}


def compare_schema(old, new) -> dict:
    """A provider schema: implementations added or gone, and per
    implementation the fields added or gone."""
    if not isinstance(old, list) or not isinstance(new, list):
        return compare_json(old, new)

    def by_impl(items):
        out = {}
        for item in items:
            if isinstance(item, dict):
                key = item.get("implementation") or item.get("name") or json.dumps(item, sort_keys=True)[:40]
                out[str(key)] = item
        return out

    o, n = by_impl(old), by_impl(new)
    fields = {}
    changed = []
    for impl in sorted(o.keys() & n.keys()):
        of = {f.get("name") for f in o[impl].get("fields", []) if isinstance(f, dict)}
        nf = {f.get("name") for f in n[impl].get("fields", []) if isinstance(f, dict)}
        if of != nf:
            fields[impl] = {"added": sorted(map(str, nf - of)), "removed": sorted(map(str, of - nf))}
        elif normalise(o[impl]) != normalise(n[impl]):
            changed.append(impl)
    result = {
        "implementations_added": sorted(n.keys() - o.keys()),
        "implementations_removed": sorted(o.keys() - n.keys()),
        "fields": fields,
        "changed": changed,
    }
    same = not any(result.values())
    result["kind"] = "same" if same else ("shape" if result["implementations_added"] or
                                          result["implementations_removed"] or fields else "values")
    return result


def compare_strings(old: dict | None, new: dict | None) -> dict:
    """The localisation file: keys added, gone, reworded."""
    old, new = old or {}, new or {}
    added = sorted(new.keys() - old.keys())
    removed = sorted(old.keys() - new.keys())
    changed = sorted(k for k in old.keys() & new.keys() if old[k] != new[k])
    kind = "same" if not (added or removed or changed) else ("shape" if added or removed else "values")
    return {"kind": kind, "added": added, "removed": removed, "changed": changed}


def compare_vectors(old: dict | None, new: dict | None) -> dict:
    """One fixture's vectors: methods added or gone, cases added or gone
    per method."""
    if old is None:
        return {"kind": "new", "methods": sorted((new or {}).get("methods", {}))}
    if new is None:
        return {"kind": "gone", "methods": sorted(old.get("methods", {}))}
    om, nm = old.get("methods", {}), new.get("methods", {})
    methods = {}
    for name in sorted(om.keys() & nm.keys()):
        oc = {json.dumps(c, sort_keys=True) for c in om[name].get("cases", [])}
        nc = {json.dumps(c, sort_keys=True) for c in nm[name].get("cases", [])}
        params_changed = om[name].get("params") != nm[name].get("params")
        if oc != nc or params_changed:
            methods[name] = {"added": len(nc - oc), "removed": len(oc - nc), "params": params_changed,
                             "examples_added": sorted(nc - oc)[:3], "examples_removed": sorted(oc - nc)[:3]}
    result = {"methods_added": sorted(nm.keys() - om.keys()), "methods_removed": sorted(om.keys() - nm.keys()),
              "methods": methods, "source": [old.get("source"), new.get("source")]}
    result["kind"] = "same" if not (result["methods_added"] or result["methods_removed"] or methods) else "changed"
    return result


def area_of(name: str) -> str:
    """A golden's area, the first word of its name: `downloadclient`, `queue`."""
    if name == "signalr.json":
        return "hub"
    stem = name.removesuffix(".json")
    for prefix in ("noauth-", "session-", "apikey-query-"):
        stem = stem.removeprefix(prefix)
    return stem.split("-")[0]


def head_files(prefix: str) -> dict[str, bytes]:
    """The files under `prefix` as HEAD has them: what a change is measured
    against, so that applying twice reports the same."""
    done = subprocess.run(["git", "-C", str(REPO), "archive", "--format=tar", "HEAD", "--", prefix],
                          capture_output=True)
    if done.returncode != 0:
        return {}
    out = {}
    with tarfile.open(fileobj=io.BytesIO(done.stdout)) as tar:
        for member in tar.getmembers():
            if member.isfile():
                out[member.name] = tar.extractfile(member).read()
    return out


def load(raw: bytes | None, kind: str = "json"):
    if raw is None:
        return None
    text = raw.decode("utf-8")
    return json.loads(text) if kind == "json" else text


def checked_file(path: Path, json_file: bool) -> bytes:
    """A file of the work directory, read as untrusted: a regular file, not
    a link, of a sane size, and JSON when it should be."""
    if path.is_symlink() or not path.is_file():
        die(f"{path} is not a regular file")
    if path.stat().st_size > MAX_FILE:
        die(f"{path} is larger than {MAX_FILE} bytes")
    raw = path.read_bytes()
    if json_file:
        try:
            json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as err:
            die(f"{path} is not JSON: {err}")
    return raw


# --- cargo and unittest output ----------------------------------------------


RUNNING = re.compile(r"^\s*Running (?P<what>\S+) \((?P<bin>[^)]+)\)")
UNIT = re.compile(r"^\s*Running unittests (?P<src>\S+) \((?P<bin>[^)]+)\)")
DOCTESTS = re.compile(r"^\s*Doc-tests (?P<crate>\S+)")
TEST_LINE = re.compile(r"^test (?P<name>\S+) \.\.\. (?P<result>ok|FAILED|ignored)")
RESULT = re.compile(r"^test result: (?P<result>\w+)\. (?P<passed>\d+) passed; (?P<failed>\d+) failed")
PY_FAIL = re.compile(r"^(?P<kind>FAIL|ERROR): (?P<name>\S+) \((?P<where>[^)]+)\)")


def binary_target(line: str) -> dict | None:
    """The cargo target of a `Running ...` line, as `cargo test` arguments
    that run it alone."""
    unit = UNIT.match(line)
    if unit:
        stem = Path(unit.group("bin")).name.rsplit("-", 1)[0]
        package = stem.replace("_", "-")
        if unit.group("src").endswith("main.rs"):
            return {"target": f"{package} (bin)", "args": ["-p", package, "--bin", package]}
        return {"target": f"{package} (lib)", "args": ["-p", package, "--lib"]}
    running = RUNNING.match(line)
    if running:
        stem = Path(running.group("what")).stem
        return {"target": f"tests/{stem}", "args": ["--workspace", "--test", stem]}
    doc = DOCTESTS.match(line)
    if doc:
        package = doc.group("crate").replace("_", "-")
        return {"target": f"{package} (doc)", "args": ["-p", package, "--doc"]}
    return None


def parse_cargo(output: str) -> dict:
    """Failing tests of a `cargo test --no-fail-fast` run, with the first
    lines of what each printed, and whether everything compiled."""
    target = None
    failures: list[dict] = []
    by_name: dict[tuple[str, str], dict] = {}
    passed = failed = 0
    lines = output.splitlines()
    capture: dict | None = None
    compile_errors = [line for line in lines if line.startswith("error[") or
                      line.startswith("error: could not compile")]
    for line in lines:
        t = binary_target(line)
        if t:
            target, capture = t, None
            continue
        m = TEST_LINE.match(line)
        if m and m.group("result") == "FAILED" and target:
            entry = {"name": m.group("name"), "target": target["target"], "args": target["args"], "output": []}
            failures.append(entry)
            by_name[(target["target"], m.group("name"))] = entry
            continue
        m = RESULT.match(line)
        if m:
            passed += int(m.group("passed"))
            failed += int(m.group("failed"))
            capture = None
            continue
        m = re.match(r"^---- (\S+) stdout ----$", line)
        if m and target:
            capture = by_name.get((target["target"], m.group(1)))
            continue
        if line.strip() == "failures:" or line.startswith("successes:"):
            capture = None
            continue
        if capture is not None and len(capture["output"]) < 60:
            capture["output"].append(line)
    # What cargo lists at the end of a --no-fail-fast run, one line per
    # target: also a target that failed without a test saying so (a crash).
    failed_targets = []
    for line in lines:
        m = re.match(r"^error: test failed, to rerun pass `([^`]+)`", line) or re.match(r"^\s+`(-p [^`]+)`$", line)
        if m and m.group(1) not in failed_targets:
            failed_targets.append(m.group(1))
    return {"passed": passed, "failed": failed, "failures": failures, "compile_errors": compile_errors[:20],
            "failed_targets": failed_targets}


def parse_unittest(output: str) -> list[dict]:
    out = []
    for line in output.splitlines():
        m = PY_FAIL.match(line)
        if m:
            out.append({"name": m.group("name"), "target": f"scripts ({m.group('where')})", "kind": m.group("kind")})
    return out


# --- the steps --------------------------------------------------------------


@dataclass
class Context:
    app: App
    work: Path
    dry_run: bool = False
    releases: str = "api"
    target: str | None = None
    keep_cursor: bool = False
    baseline: bool = False
    rerun: int = 20
    lists: list[str] | None = None
    timings: dict[str, float] = field(default_factory=dict)

    def path(self, *parts: str) -> Path:
        return self.work.joinpath(*parts)

    def bump(self) -> dict:
        path = self.path("bump.json")
        if not path.exists():
            die(f"no {path}: run the fetch step first")
        bump = read_json(path)
        if bump.get("app") != self.app.key:
            die(f"{path} is a bump of {bump.get('app')}, not {self.app.key}")
        triage.Version.parse(bump["to"]["version"])
        if not SHA256.match(bump["sha256"]) or not triage.SHA.match(bump["to"]["commit"]):
            die(f"{path}: odd checksum or commit")
        return bump


def asset_url(app: App, lock, version: str) -> tuple[str, str]:
    """The release URL of the tarball on the project's GitHub page, and
    the asset's file name."""
    slug = triage.github_slug(lock.repo)
    name = app.asset.format(branch=lock.branch, version=version)
    return f"https://github.com/{slug}/releases/download/v{version}/{name}", name


def check_asset_host(url: str) -> str:
    host = urllib.parse.urlsplit(url).hostname or ""
    if urllib.parse.urlsplit(url).scheme != "https" or host not in ASSET_HOSTS:
        die(f"the download ended at {host or url!r}, not at GitHub's release storage ({', '.join(sorted(ASSET_HOSTS))})")
    return host


def check_tarball(path: Path, app: App) -> int:
    """The archive's entries, before anything extracts it: all under
    `<App>/`, no absolute path, no `..`, no device; the binary there."""
    entries = 0
    binary = False
    with tarfile.open(path, "r:gz") as tar:
        for member in tar:
            entries += 1
            name = member.name
            if name.startswith("/") or ".." in Path(name).parts or not (name == app.name or
                                                                         name.startswith(app.name + "/")):
                die(f"{path.name}: unexpected entry {name!r}")
            if member.isdev() or member.isfifo():
                die(f"{path.name}: device entry {name!r}")
            if member.issym() or member.islnk():
                target = Path(name).parent / member.linkname
                if member.linkname.startswith("/") or ".." in target.parts:
                    die(f"{path.name}: link {name!r} points outside the archive")
            if name == f"{app.name}/{app.name}" and member.isfile():
                binary = True
    if not binary:
        die(f"{path.name}: no {app.name}/{app.name} in the archive")
    return entries


def api_digest(releases_json: Path | None, tag: str, asset: str) -> str | None:
    """The `sha256:` digest the GitHub API gives for the asset, when known."""
    if releases_json is None or not releases_json.exists():
        return None
    stack = [read_json(releases_json)]
    while stack:
        item = stack.pop()
        if isinstance(item, list):
            stack.extend(item)
        elif isinstance(item, dict) and item.get("tag_name") == tag:
            for a in item.get("assets", []):
                if a.get("name") == asset and isinstance(a.get("digest"), str):
                    return a["digest"]
    return None


def step_fetch(ctx: Context) -> None:
    app = ctx.app
    lock = triage.read_lock(UPSTREAM / f"{app.key}.lock")
    wontport = triage.read_wontport(UPSTREAM / f"{app.key}-wontport.md")
    repo_dir = cache_dir() / "upstream" / f"{app.name}.git"
    triage.fetch(repo_dir, lock)
    report, commits = triage.triage(app.key, repo_dir, lock, wontport, ctx.target, ctx.releases)
    if report["target"] is None:
        log(f"no stable release newer than {lock.release}: nothing to bump")
        raise SystemExit(0)
    version, tag = report["target"], report["target_tag"]
    commit = triage.git(repo_dir, "rev-parse", f"{tag}^{{commit}}").strip()
    url, name = asset_url(app, lock, version)
    releases_json = {"api": cache_dir() / "upstream" / f"{app.key}-releases.json",
                     "tags": None}.get(ctx.releases, Path(ctx.releases))
    if ctx.dry_run:
        print(f"fetch: {app.name} {lock.release} -> {version} ({report['jump']}), tag {tag} at {commit[:12]}, "
              f"{len(commits)} upstream commits ({report['releases_from']})")
        print(f"  download {url}")
        print(f"  into {ctx.path('original', name)}; checksum, entries checked; the GitHub API's digest "
              + ("compared" if releases_json else "not available (RELEASES=tags)"))
        print(f"  writes {ctx.path('bump.json')}, triage.json, triage.md")
        return
    old = ctx.path("bump.json")
    if old.exists() and read_json(old).get("to", {}).get("version") != version:
        log(f"{ctx.work} held a bump to another release: emptied")
        shutil.rmtree(ctx.work)
    ctx.work.mkdir(parents=True, exist_ok=True)
    dest = ctx.path("original", name)
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_suffix(dest.suffix + ".part")
    log(f"downloading {url}")
    done = subprocess.run(["curl", "-fsSL", "--proto", "=https", "--proto-redir", "=https", "--max-redirs", "5",
                           "-o", str(part), "-w", "%{url_effective}", url], capture_output=True, text=True)
    if done.returncode != 0:
        part.unlink(missing_ok=True)
        die(f"the download of {url} failed: {done.stderr.strip()}")
    host = check_asset_host(done.stdout.strip())
    part.replace(dest)
    digest = sha256_file(dest)
    entries = check_tarball(dest, app)
    published = api_digest(releases_json, tag, name)
    if published is not None and published != f"sha256:{digest}":
        die(f"{name}: SHA-256 {digest}, the GitHub API says {published}")
    bump = {
        "app": app.key,
        "from": {"version": str(lock.release), "commit": lock.commit},
        "to": {"version": version, "tag": tag, "commit": commit},
        "jump": report["jump"],
        "releases_from": report["releases_from"],
        "url": url,
        "served_from": host,
        "asset": name,
        "sha256": digest,
        "size": dest.stat().st_size,
        "entries": entries,
        "api_digest": "matched" if published else ("not published by the API" if releases_json
                                                   else "not read: RELEASES=tags"),
        "fetched_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    write_json(ctx.path("bump.json"), bump)
    report["commits"] = [
        {"sha": c.sha, "date": c.date, "subject": c.subject, "class": c.cls, "also": c.also,
         "migrations": c.migrations, "migrations_changed": c.migrations_changed,
         "migrations_removed": c.migrations_removed,
         "paths": [{"status": s, "path": p} for s, p in c.changes]}
        for c in commits
    ]
    write_json(ctx.path("triage.json"), report)
    ctx.path("triage.md").write_text(triage.render_markdown(report, commits))
    log(f"{name}: {bump['size']} bytes from {host}, sha256 {digest}; API digest {bump['api_digest']}")


def fixture_env(ctx: Context, version: str, sha: str, cache: Path) -> dict:
    env = dict(os.environ)
    env.update({"UNARR_CACHE": str(cache), ctx.app.version_env: version, ctx.app.sha_env: sha})
    for var in HEADER_VARIABLES | {"SCHEMAS_OUT", "RECORD_METADATA"}:
        env.pop(var, None)
    return env


def migration_level(db: Path) -> int:
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        (level,) = con.execute('SELECT MAX("Version") FROM "VersionInfo"').fetchone()
    finally:
        con.close()
    if not isinstance(level, int):
        die(f"{db}: no migration level in VersionInfo")
    return level


def record_with(ctx: Context, version: str, sha: str, cache: Path, out_root: Path, logs: Path,
                lists: list[GoldenList], migrate: bool) -> dict:
    """Records every list with the original at `version` (its tarball in
    `cache`, or downloaded there by the fixture script)."""
    app = ctx.app
    base_env = fixture_env(ctx, version, sha, cache)
    script = ["bash", str(app.fixture_script), "golden"]
    result: dict = {"version": version, "lists": []}
    data = cache / "golden-data"
    if migrate:
        empty = ctx.path("migrate", "requests.txt")
        empty.parent.mkdir(parents=True, exist_ok=True)
        empty.write_text("# No request: the original starts on a copy of the fixture, migrates it and stops.\n")
        env = dict(base_env, GOLDEN_REQUESTS=str(empty), GOLDEN_OUT=str(ctx.path("migrate", "out")))
        started = time.monotonic()
        code = run(script, logs / "migrate.log", env)
        if code != 0:
            die(f"the original did not start on the fixture (exit {code}), see {logs / 'migrate.log'}")
        migrated = ctx.path("migrated", app.db)
        migrated.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(data / app.db, migrated)
        result["migration"] = migration_level(migrated)
        result["migrate_seconds"] = round(time.monotonic() - started, 1)
        log(f"the fixture migrated to {result['migration']}, kept in {migrated}")
    for g in lists:
        out = out_root / g.key
        if out.exists():
            shutil.rmtree(out)
        env = dict(base_env, GOLDEN_REQUESTS=str(g.dir / "requests.txt"), GOLDEN_OUT=str(out))
        if "GOLDEN_SETUP" in g.env:
            env["GOLDEN_SETUP"] = str(REPO / g.env["GOLDEN_SETUP"])
        if "GOLDEN_SIGNALR" in g.env:
            env["GOLDEN_SIGNALR"] = g.env["GOLDEN_SIGNALR"]
        if g.key == "golden":
            env["SCHEMAS_OUT"] = str(out_root / "schemas")
        log(f"recording {g.key} ({len(g.names)} answers) with {app.name} {version}")
        started = time.monotonic()
        code = run(script, logs / f"{g.key}.log", env)
        entry = {"list": g.key, "exit": code, "seconds": round(time.monotonic() - started, 1)}
        if code == 0:
            if (g.dir / "db-after.json").exists():
                run([sys.executable, "-I", str(SCRIPTS / "housekeeping-snapshot.py"), str(data / app.db),
                     str(out / "db-after.json")], logs / f"{g.key}.log")
            if (g.dir / "config-after.xml").exists():
                shutil.copyfile(data / "config.xml", out / "config-after.xml")
        for name in ("mock-requests.jsonl",):
            if (cache / name).exists():
                shutil.copyfile(cache / name, logs / f"{g.key}.{name}")
        stdout_log = data / f"{app.key}.stdout.log"
        if stdout_log.exists():
            shutil.copyfile(stdout_log, logs / f"{g.key}.{app.key}.stdout.log")
        missing = [f for f in g.outputs(app) if not (out / f).exists()]
        entry["missing"] = missing
        entry["ok"] = code == 0 and not missing
        result["lists"].append(entry)
        log(f"{g.key}: {'recorded' if entry['ok'] else 'FAILED'} in {entry['seconds']}s"
            + (f", {len(missing)} files missing" if missing else ""))
    status = out_root / "golden" / "system-status.json"
    if status.exists():
        body = read_json(status).get("body") or {}
        result["build_time"] = body.get("buildTime")
        result["reported_version"] = body.get("version")
    return result


def metadata_misses(logs: Path) -> dict[str, list[str]]:
    """Per list, the metadata requests the mock had no recorded answer for."""
    out: dict[str, list[str]] = {}
    for path in sorted(logs.glob("*.mock-requests.jsonl")):
        misses = []
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            served = str(entry.get("served", ""))
            if served.endswith("(missing)"):
                misses.append(f"{entry.get('method')} {served.removesuffix(' (missing)')}")
        if misses:
            out[path.name.split(".")[0]] = sorted(set(misses))
    return out


def step_record(ctx: Context) -> None:
    app = ctx.app
    lists = golden_lists(app, ctx.lists)
    if ctx.dry_run:
        print(f"record: runs {app.name} from the work directory's tarball ({ctx.path('original')}), "
              f"{app.fixture_script.relative_to(REPO)} golden with UNARR_CACHE={ctx.path('original')}")
        print("  0. no request: the fixture migrated, its level read from VersionInfo")
        for i, g in enumerate(lists, 1):
            extra = " ".join(f"{k}={v}" for k, v in sorted(g.env.items()) if k in ("GOLDEN_SETUP", "GOLDEN_SIGNALR"))
            more = ", the provider schemas and the localisation file" if g.key == "golden" else ""
            print(f"  {i}. {g.key}: {len(g.names)} answers{more}" + (f" ({extra})" if extra else ""))
        print(f"  into {ctx.path('recorded')}; nothing in the repository"
              + (f"; --baseline: the same with the pinned release into {ctx.path('baseline')}" if ctx.baseline else ""))
        return
    bump = ctx.bump()
    tarball = ctx.path("original", bump["asset"])
    if not tarball.exists() or sha256_file(tarball) != bump["sha256"]:
        die(f"{tarball} is missing or not the one fetched: run the fetch step again")
    started = time.monotonic()
    result = record_with(ctx, bump["to"]["version"], bump["sha256"], ctx.path("original"), ctx.path("recorded"),
                         ctx.path("logs"), lists, migrate=True)
    result["metadata_misses"] = metadata_misses(ctx.path("logs"))
    if ctx.baseline:
        pin = read_pin(UPSTREAM / f"{app.key}.pin")
        cache = ctx.path("baseline-original")
        cache.mkdir(parents=True, exist_ok=True)
        pinned = fixture_cache(app) / app.asset.format(branch=triage.read_lock(UPSTREAM / f"{app.key}.lock").branch,
                                                       version=pin["version"])
        if pinned.exists():
            shutil.copyfile(pinned, cache / pinned.name)
        base = record_with(ctx, pin["version"], pin["sha256"], cache, ctx.path("baseline"), ctx.path("baseline-logs"),
                           lists, migrate=False)
        base["metadata_misses"] = metadata_misses(ctx.path("baseline-logs"))
        result["baseline"] = base
    result["seconds"] = round(time.monotonic() - started, 1)
    if result.get("reported_version") != bump["to"]["version"]:
        log(f"warning: system/status says {result.get('reported_version')!r}, not {bump['to']['version']}")
    write_json(ctx.path("record.json"), result)
    failed = [e["list"] for e in result["lists"] if not e["ok"]]
    log(f"recorded in {result['seconds']}s" + (f"; failed: {', '.join(failed)}" if failed else ""))


def recorded_changes(ctx: Context, record: dict, root: str = "recorded") -> dict:
    """Each recorded list compared with HEAD's files, and the files to copy."""
    app = ctx.app
    lists = {g.key: g for g in golden_lists(app)}
    head = head_files(app.fixtures.relative_to(REPO).as_posix())
    changes: dict = {"lists": {}, "copy": []}
    for entry in record.get("lists", []) if root == "recorded" else record.get("baseline", {}).get("lists", []):
        g = lists.get(entry["list"])
        if g is None or not entry.get("ok"):
            continue
        files = {}
        for name in g.outputs(app):
            src = ctx.path(root, g.key, name)
            rel = (g.dir / name).relative_to(REPO).as_posix()
            old_raw = head.get(rel)
            if not src.exists():
                files[name] = {"kind": "not recorded"}
                continue
            is_json = name.endswith(".json")
            new_raw = checked_file(src, is_json)
            if is_json:
                files[name] = compare_json(load(old_raw), load(new_raw))
            else:
                files[name] = compare_text(load(old_raw, "text"), load(new_raw, "text"))
            changes["copy"].append((str(src), str(g.dir / name)))
        changes["lists"][g.key] = files
    return changes


def schema_changes(ctx: Context, root: str = "recorded") -> dict:
    app = ctx.app
    head = head_files(app.schemas)
    out: dict = {"files": {}, "copy": []}
    for rel, raw in sorted(head.items()):
        name = Path(rel).name
        if not (re.match(r"^[a-z]+-schema\.json$", name) or name == "localization-en.json"):
            continue
        src = ctx.path(root, "schemas", name)
        if not src.exists():
            out["files"][name] = {"kind": "not recorded"}
            continue
        new = load(checked_file(src, True))
        old = load(raw)
        out["files"][name] = compare_strings(old, new) if name == "localization-en.json" else compare_schema(old, new)
        out["copy"].append((str(src), str(REPO / rel)))
    return out


def importer_range(app: App, migration: int) -> dict:
    path = REPO / app.importer
    for n, line in enumerate(path.read_text().splitlines(), 1):
        m = re.match(r"^pub const MAX_KNOWN_MIGRATION: i64 = (\d+);", line)
        if m:
            known = int(m.group(1))
            return {"file": app.importer, "line": n, "known": known, "migration": migration,
                    "raise": migration > known}
    die(f"no MAX_KNOWN_MIGRATION in {app.importer}")


def source_of_tag(ctx: Context, tag: str) -> Path:
    """The upstream test directories at `tag`, from the triage's clone,
    extracted with the `data` filter (untrusted names stay inside)."""
    repo_dir = cache_dir() / "upstream" / f"{ctx.app.name}.git"
    dest = ctx.path("source")
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    paths = [f"src/NzbDrone.Core.Test/{area}" for area in VECTOR_AREAS]
    done = subprocess.run(["git", "-C", str(repo_dir), "archive", "--format=tar", tag, "--", *paths],
                          capture_output=True)
    if done.returncode != 0:
        die(f"cannot read {', '.join(paths)} at {tag} from {repo_dir}: {done.stderr.decode(errors='replace')}")
    with tarfile.open(fileobj=io.BytesIO(done.stdout)) as tar:
        tar.extractall(dest, filter="data")
    return dest


def vector_changes(ctx: Context, source: Path) -> dict:
    """The vectors of the whole tag, written apart and compared with HEAD's."""
    app = ctx.app
    out_dir = ctx.path("vectors")
    if out_dir.exists():
        shutil.rmtree(out_dir)
    cmd = [sys.executable, "-I", str(SCRIPTS / "port-vectors.py"), "--source", str(source), "--out", str(out_dir)]
    for area in VECTOR_AREAS:
        cmd += ["--area", area]
    if run(cmd, ctx.path("logs", "port-vectors.log")) != 0:
        die(f"scripts/port-vectors.py failed, see {ctx.path('logs', 'port-vectors.log')}")
    result: dict = {"files": {}, "write": [], "delete": []}
    for area_dir in VECTOR_AREAS.values():
        rel = f"{app.vectors}/{area_dir}"
        head = {Path(p).name: raw for p, raw in head_files(rel).items() if p.endswith(".json")}
        new = {p.name: p for p in sorted((out_dir / area_dir).glob("*.json"))}
        for name in sorted(head.keys() | new.keys()):
            old_data = load(head.get(name))
            new_data = load(new[name].read_bytes()) if name in new else None
            change = compare_vectors(old_data, new_data)
            if change["kind"] != "same":
                result["files"][f"{area_dir}/{name}"] = change
            if name in new:
                result["write"].append((str(new[name]), str(REPO / rel / name)))
            else:
                result["delete"].append(str(REPO / rel / name))
    return result


def step_apply(ctx: Context) -> None:
    app = ctx.app
    pin_path, lock_path = UPSTREAM / f"{app.key}.pin", UPSTREAM / f"{app.key}.lock"
    if ctx.dry_run:
        print(f"apply: writes {pin_path.relative_to(REPO)} (version, build_time, migration, sha256)"
              + ("" if ctx.keep_cursor else f" and {lock_path.relative_to(REPO)} (release, commit)"))
        print(f"  copies the recordings over tests/fixtures/{app.key}/golden*/ and {app.schemas}/, "
              "after comparing them with HEAD")
        print(f"  removes {fixture_cache(app) / app.name} and runs `make ui-{app.key}` (downloads the tarball again, "
              "checked against the new pin)")
        print(f"  regenerates {app.vectors}/{{{','.join(VECTOR_AREAS.values())}}} from the tag "
              "(scripts/port-vectors.py), files gone upstream removed")
        print(f"  reads MAX_KNOWN_MIGRATION in {app.importer}; raises nothing")
        return
    bump = ctx.bump()
    record_path = ctx.path("record.json")
    if not record_path.exists():
        die(f"no {record_path}: run the record step first")
    record = read_json(record_path)
    main = next((e for e in record.get("lists", []) if e.get("list") == "golden"), None)
    if not main or not main.get("ok"):
        die("the main list (golden) was not recorded: no build time to pin, nothing applied")
    values = {"version": bump["to"]["version"], "build_time": record.get("build_time"),
              "migration": record.get("migration"), "sha256": bump["sha256"]}
    check_pin_values(values)
    if record.get("reported_version") != values["version"]:
        die(f"the recording's system/status says {record.get('reported_version')!r}, not {values['version']}")
    started = time.monotonic()
    changes = recorded_changes(ctx, record)
    schemas = schema_changes(ctx)
    baseline = None
    if record.get("baseline"):
        baseline = {"recorded": {k: v for k, v in recorded_changes(ctx, record, "baseline").items() if k != "copy"},
                    "schemas": {k: v for k, v in schema_changes(ctx, "baseline").items() if k != "copy"}}
    for src, dest in changes["copy"] + schemas["copy"]:
        Path(dest).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dest)
    log(f"copied {len(changes['copy'])} recordings and {len(schemas['copy'])} schema files")
    write_pin(pin_path, values)
    if not ctx.keep_cursor:
        write_lock(lock_path, bump["to"]["version"], bump["to"]["commit"])
    log(f"pinned {app.name} {values['version']} (build {values['build_time']}, migration {values['migration']})"
        + ("" if ctx.keep_cursor else f"; cursor at {bump['to']['commit'][:12]}"))
    binary = fixture_cache(app) / app.name
    if binary.exists():
        shutil.rmtree(binary)
        log(f"removed {binary}")
    ui_started = time.monotonic()
    code = run(["make", f"ui-{app.key}"], ctx.path("logs", "make-ui.log"))
    if code != 0:
        die(f"make ui-{app.key} failed (exit {code}), see {ctx.path('logs', 'make-ui.log')}")
    ui_seconds = round(time.monotonic() - ui_started, 1)
    source = source_of_tag(ctx, bump["to"]["tag"])
    vectors = vector_changes(ctx, source)
    for src, dest in vectors["write"]:
        Path(dest).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dest)
    for dest in vectors["delete"]:
        Path(dest).unlink(missing_ok=True)
    importer = importer_range(app, values["migration"])
    applied = {
        "pin": values,
        "cursor": None if ctx.keep_cursor else {"release": bump["to"]["version"], "commit": bump["to"]["commit"]},
        "recordings": changes["lists"],
        "schemas": schemas["files"],
        "baseline": baseline,
        "vectors": {"files": vectors["files"], "written": len(vectors["write"]),
                    "deleted": [str(Path(p).relative_to(REPO)) for p in vectors["delete"]]},
        "importer": importer,
        "ui_seconds": ui_seconds,
        "seconds": round(time.monotonic() - started, 1),
    }
    write_json(ctx.path("apply.json"), applied)
    log(f"applied in {applied['seconds']}s")


def step_test(ctx: Context) -> None:
    if ctx.dry_run:
        print("test: cargo test --workspace --no-fail-fast, make test-scripts; "
              f"each failure run once more (up to {ctx.rerun})")
        return
    started = time.monotonic()
    logs = ctx.path("logs")
    cargo_log = logs / "cargo-test.log"
    cargo_log.unlink(missing_ok=True)
    code = run(["cargo", "test", "--workspace", "--no-fail-fast"], cargo_log)
    cargo = parse_cargo(cargo_log.read_text(errors="replace"))
    cargo["exit"] = code
    scripts_log = logs / "test-scripts.log"
    scripts_log.unlink(missing_ok=True)
    scripts_code = run(["make", "test-scripts"], scripts_log)
    scripts = parse_unittest(scripts_log.read_text(errors="replace"))
    reruns = 0
    for failure in cargo["failures"]:
        if reruns >= ctx.rerun or failure["target"].endswith("(doc)"):
            continue
        reruns += 1
        rerun_log = logs / "rerun.log"
        cmd = ["cargo", "test", *failure["args"], "--", "--exact", failure["name"]]
        failure["rerun"] = "passed" if run(cmd, rerun_log) == 0 else "failed"
        if failure["rerun"] == "passed":
            log(f"{failure['name']} passed when run again: flaky, not broken")
    result = {"cargo": cargo, "scripts": {"exit": scripts_code, "failures": scripts},
              "seconds": round(time.monotonic() - started, 1)}
    write_json(ctx.path("tests.json"), result)
    log(f"tests: {cargo['passed']} passed, {cargo['failed']} failed; scripts: {len(scripts)} failed "
        f"({result['seconds']}s)")


# --- the summary ------------------------------------------------------------


def md(text: str, limit: int = 160) -> str:
    return triage.md_code(str(text), limit)


def kinds_table(lists: dict, baseline: dict | None) -> list[str]:
    kinds = ("same", "time", "values", "shape", "status", "new", "gone", "not recorded")
    lines = ["| List | Files | " + " | ".join(kinds) + " |", "|---|---:|" + "---:|" * len(kinds)]
    for key, files in lists.items():
        counts = {k: 0 for k in kinds}
        for change in files.values():
            counts[change["kind"]] = counts.get(change["kind"], 0) + 1
        lines.append(f"| `{key}` | {len(files)} | " + " | ".join(str(counts[k]) for k in kinds) + " |")
    return lines


def noisy(baseline: dict | None, key: str, name: str) -> bool:
    """Whether a re-record of the pinned release changed the file too."""
    if not baseline:
        return False
    change = baseline["recorded"]["lists"].get(key, {}).get(name)
    return bool(change) and change["kind"] not in ("same", "time")


def render_summary(work: Path) -> tuple[str, dict]:
    bump = read_json(work / "bump.json")
    tri = read_json(work / "triage.json") if (work / "triage.json").exists() else {}
    record = read_json(work / "record.json") if (work / "record.json").exists() else {}
    applied = read_json(work / "apply.json") if (work / "apply.json").exists() else {}
    tests = read_json(work / "tests.json") if (work / "tests.json").exists() else {}
    app = APPS[bump["app"]]
    lines = [f"# {app.name} {bump['from']['version']} → {bump['to']['version']} ({bump['jump']})", ""]
    data: dict = {"app": app.key, "from": bump["from"], "to": bump["to"], "jump": bump["jump"]}

    lines += ["## The new original", ""]
    lines.append(f"- Tag `{bump['to']['tag']}` at `{bump['to']['commit']}`; stable releases from "
                 f"{bump['releases_from']}.")
    lines.append(f"- Tarball {bump['url']}, served from `{bump['served_from']}`, {bump['size']} bytes, "
                 f"{bump['entries']} entries, SHA-256 `{bump['sha256']}`; the GitHub API's digest: "
                 f"{bump['api_digest']}.")
    if record:
        lines.append(f"- Build time `{record.get('build_time')}`, migration level {record.get('migration')} "
                     f"(read from the fixture the new original migrated; the database is kept in "
                     f"`{work / 'migrated' / app.db}`).")
    if tri:
        counts = ", ".join(f"{n} {c}" for c, n in tri.get("summary", {}).items() if n)
        lines.append(f"- Upstream: {len(tri.get('commits', []))} commits ({counts}); migrations added: "
                     f"{', '.join(map(str, tri.get('migrations', []))) or 'none'}. The table is in "
                     f"`{work / 'triage.md'}`.")
    lines.append("")

    if applied:
        lines += ["## Written", ""]
        lines.append(f"- `upstream/{app.key}.pin`: " + ", ".join(f"{k} `{v}`" for k, v in applied["pin"].items()))
        if applied.get("cursor"):
            lines.append(f"- `upstream/{app.key}.lock`: release `{applied['cursor']['release']}`, commit "
                         f"`{applied['cursor']['commit']}`")
        else:
            lines.append(f"- `upstream/{app.key}.lock`: left where it was (--keep-cursor)")
        lines.append(f"- the web UI of the new tarball (`make ui-{app.key}`, {applied.get('ui_seconds')}s)")
        lines.append("")

        rec = applied["recordings"]
        baseline = applied.get("baseline")
        total = sum(len(f) for f in rec.values())
        changed = {(k, n): c for k, files in rec.items() for n, c in files.items()
                   if c["kind"] not in ("same", "time")}
        lines += ["## Recordings", ""]
        lines.append(f"{total} files in {len(rec)} lists; {len(changed)} changed beyond the instants "
                     "(`time`: only dates and times differ, as on any recording)"
                     + (", those that also change on a re-record of the pinned release marked *noisy*" if baseline
                        else "; no baseline recorded (BASELINE=1), so a change here can also be one any "
                             "recording makes") + ".")
        lines.append("")
        lines += kinds_table(rec, baseline)
        lines.append("")
        failed = [e for e in record.get("lists", []) if not e.get("ok")]
        if failed:
            lines.append("Lists not recorded: " + ", ".join(
                f"`{e['list']}` (exit {e['exit']}, missing {len(e.get('missing', []))})" for e in failed) + ".")
            lines.append("")
        areas: dict[str, dict[str, int]] = {}
        for (key, name), change in changed.items():
            area = areas.setdefault(area_of(name), {})
            area[change["kind"]] = area.get(change["kind"], 0) + 1
        if areas:
            lines += ["By area (changed files, by kind):", "", "| Area | Changed | Kinds |", "|---|---:|---|"]
            for area, kinds in sorted(areas.items(), key=lambda kv: (-sum(kv[1].values()), kv[0])):
                lines.append(f"| {area} | {sum(kinds.values())} | "
                             + ", ".join(f"{k} {n}" for k, n in sorted(kinds.items())) + " |")
            lines.append("")
            lines += ["Changed files (first differences):", ""]
            for (key, name), change in sorted(changed.items()):
                flag = " *noisy*" if noisy(baseline, key, name) else ""
                detail = []
                if change["kind"] == "status":
                    detail.append(f"status {change['status'][0]} → {change['status'][1]}")
                for p in change.get("added", [])[:4]:
                    detail.append(f"+{p}")
                for p in change.get("removed", [])[:4]:
                    detail.append(f"-{p}")
                if change["kind"] in ("values", "status") or not detail:
                    detail += change.get("values", [])[:3]
                lines.append(f"- `{key}/{name}` {change['kind']}{flag}: "
                             + ("; ".join(md(d, 140) for d in detail) if detail else ""))
            lines.append("")
        data["recordings"] = {"files": total, "changed": len(changed), "areas": areas,
                              "failed_lists": [e["list"] for e in failed]}

        lines += ["## Schemas and localisation", ""]
        for name, change in applied["schemas"].items():
            if change["kind"] == "same":
                lines.append(f"- `{name}`: unchanged")
            elif name == "localization-en.json":
                lines.append(f"- `{name}`: {len(change['added'])} strings added, {len(change['removed'])} gone, "
                             f"{len(change['changed'])} reworded"
                             + (f"; added e.g. {', '.join(md(k, 60) for k in change['added'][:6])}"
                                if change["added"] else ""))
            elif change["kind"] == "not recorded":
                lines.append(f"- `{name}`: not recorded")
            else:
                parts = []
                if change.get("implementations_added"):
                    parts.append("implementations added " + ", ".join(md(i, 60) for i in change["implementations_added"]))
                if change.get("implementations_removed"):
                    parts.append("gone " + ", ".join(md(i, 60) for i in change["implementations_removed"]))
                for impl, f in list(change.get("fields", {}).items())[:8]:
                    parts.append(f"{md(impl, 60)} fields +{len(f['added'])}/-{len(f['removed'])}"
                                 + (f" ({', '.join(md(x, 40) for x in (f['added'] + f['removed'])[:4])})"))
                if change.get("changed"):
                    parts.append(f"{len(change['changed'])} implementations with other values (labels, help, options)")
                lines.append(f"- `{name}`: " + "; ".join(parts))
        lines.append("")

        vec = applied["vectors"]
        lines += ["## Test vectors", ""]
        lines.append(f"`scripts/port-vectors.py` on the whole of `{bump['to']['tag']}`: {vec['written']} files "
                     f"written, {len(vec['files'])} changed"
                     + (f", {len(vec['deleted'])} gone upstream and removed" if vec["deleted"] else "") + ".")
        for name, change in sorted(vec["files"].items()):
            if change["kind"] in ("new", "gone"):
                lines.append(f"- `{name}`: {change['kind']} ({len(change['methods'])} methods)")
                continue
            parts = []
            if change["methods_added"]:
                parts.append("methods added " + ", ".join(md(m, 60) for m in change["methods_added"]))
            if change["methods_removed"]:
                parts.append("gone " + ", ".join(md(m, 60) for m in change["methods_removed"]))
            for method, m in change["methods"].items():
                parts.append(f"{md(method, 60)} +{m['added']}/-{m['removed']} cases"
                             + (" (parameters changed)" if m["params"] else ""))
            lines.append(f"- `{name}`: " + "; ".join(parts))
        lines.append("")

        imp = applied["importer"]
        lines += ["## The importer", ""]
        if imp["raise"]:
            added = tri.get("migrations", [])
            files = sorted({p["path"] for c in tri.get("commits", []) for p in c.get("paths", [])
                            if re.search(r"/Datastore/Migration/\d+_", p["path"])})
            lines.append(f"`MAX_KNOWN_MIGRATION` is {imp['known']} at `{imp['file']}:{imp['line']}`, the new original "
                         f"migrates to {imp['migration']}: raise it to {imp['migration']} once the mapping is checked "
                         f"against the migrations added ({', '.join(map(str, added)) or 'none listed'}), and add a "
                         f"test that imports a database at {imp['migration']} (the fixture the new original "
                         f"migrated is `{work / 'migrated' / app.db}`). Until then "
                         "`the_pinned_release_is_within_the_known_migrations` fails. Not raised by the bump: that "
                         "test is the reminder.")
            if files:
                lines.append("")
                lines.append("Upstream migration files touched since the cursor:")
                lines += [f"- {md(f, 160)}" for f in files]
        else:
            lines.append(f"`MAX_KNOWN_MIGRATION` ({imp['known']}, `{imp['file']}:{imp['line']}`) already covers "
                         f"migration {imp['migration']}: nothing to raise.")
        lines.append("")
        data["importer"] = imp
        data["vectors"] = {"changed": len(vec["files"]), "deleted": vec["deleted"]}

    if tests:
        cargo = tests["cargo"]
        lines += ["## Tests", ""]
        lines.append(f"`cargo test --workspace`: {cargo['passed']} passed, {cargo['failed']} failed"
                     f" (exit {cargo['exit']}, {tests['seconds']}s in all).")
        if cargo.get("failed_targets"):
            lines.append("Targets that failed: " + ", ".join(md(x, 80) for x in cargo["failed_targets"])
                         + " (one with no failing test listed below ended as a whole: a crash or a timeout).")
        if cargo.get("compile_errors"):
            lines.append("")
            lines.append("Did not compile:")
            lines += [f"- {md(e, 200)}" for e in cargo["compile_errors"]]
        by_target: dict[str, list[dict]] = {}
        for f in cargo["failures"]:
            by_target.setdefault(f["target"], []).append(f)
        if by_target:
            lines += ["", "| Target | Failing tests |", "|---|---|"]
            for target, fs in sorted(by_target.items()):
                names = []
                for f in fs:
                    note = ""
                    if f.get("rerun") == "passed":
                        note = " (passed when run again: flaky)"
                    if f["name"].split("::")[-1] in KNOWN_FLAKY:
                        note += " (known flaky)"
                    names.append(md(f["name"], 100) + note)
                lines.append(f"| `{target}` | " + ", ".join(names) + " |")
            lines.append("")
            lines.append("What each failure printed first:")
            lines.append("")
            for f in cargo["failures"]:
                first = [ln for ln in f["output"] if ln.strip()][:6]
                lines.append(f"- {md(f['name'], 100)} ({f['target']}):")
                lines += [f"  - {md(ln, 200)}" for ln in first]
        for name, why in KNOWN_FLAKY.items():
            if any(f["name"].split("::")[-1] == name for f in cargo["failures"]):
                lines.append("")
                lines.append(f"`{name}` is known to fail now and then: {why}. Not a sign of this bump unless it "
                             "fails again.")
        scripts = tests["scripts"]
        lines.append("")
        lines.append(f"`make test-scripts`: exit {scripts['exit']}"
                     + (": " + ", ".join(md(f["name"], 80) + f" ({f['target']})" for f in scripts["failures"])
                        if scripts["failures"] else ""))
        lines.append("")
        data["tests"] = {"passed": cargo["passed"], "failed": cargo["failed"],
                         "failures": [{k: f[k] for k in ("name", "target") if k in f} | {"rerun": f.get("rerun")}
                                      for f in cargo["failures"]],
                         "scripts_failures": scripts["failures"]}

    lines += ["## Left to check by hand", ""]
    misses = record.get("metadata_misses", {}) if record else {}
    base_misses = (record.get("baseline") or {}).get("metadata_misses", {}) if record else {}
    if misses:
        lines.append("- Metadata (`tests/fixtures/metadata/`): the mock had no recorded answer for "
                     + "; ".join(f"`{k}` " + ", ".join(md(m, 100) for m in v[:6]) for k, v in misses.items())
                     + ". If the new original asks for something else, record it with `RECORD_METADATA=1`"
                     + (" (the pinned release's own misses, for comparison: "
                        + "; ".join(f"`{k}` {len(v)}" for k, v in base_misses.items()) + ")" if base_misses else "")
                     + ".")
    else:
        lines.append("- Metadata (`tests/fixtures/metadata/`): every request the new original made had a recorded "
                     "answer; a changed answer shape still shows only on the metadata servers' own check.")
    lines.append(f"- Recyclarr: `tests/fixtures/recyclarr/{app.key}-exchange.json` was recorded between the real "
                 "Recyclarr and the pinned original; record it again with the new one "
                 "(`tests/fixtures/recyclarr/README.md`, Recording) and run "
                 f"`make recyclarr{'' if app.key == 'sonarr' else '-radarr'}`.")
    lines.append(f"- Bazarr and Prowlarr: `scripts/bazarr-replay.py` and `scripts/prowlarr-replay.py` are written "
                 f"from those projects' source, which branches on the version `system/status` reports: check that "
                 f"the versions in use support {app.name} {bump['to']['version']} (issue #9, still to decide), "
                 f"then run `make bazarr{'' if app.key == 'sonarr' else '-radarr'}` and "
                 f"`make prowlarr{'' if app.key == 'sonarr' else '-radarr'}` against unarrd.")
    lines.append(f"- Prose that names the old release: `tests/vectors/README.md`, `tests/fixtures/{app.key}/README.md`, "
                 "`tests/fixtures/recyclarr/README.md`; `docs/originals-comparison.md` as `CLAUDE.md` says for a new pin.")
    lines.append(f"- `make ui-smoke SMOKE_APP={app.key}` on the new UI, and `scripts/route-audit.py` against the new "
                 "upstream source.")
    lines.append("")

    timings = []
    if record:
        timings.append(f"record {record.get('seconds')}s (migrate {record.get('migrate_seconds')}s; "
                       + ", ".join(f"{e['list']} {e['seconds']}s" for e in record.get("lists", [])) + ")")
        if record.get("baseline"):
            timings.append("baseline " + ", ".join(f"{e['list']} {e['seconds']}s"
                                                   for e in record["baseline"].get("lists", [])))
    if applied:
        timings.append(f"apply {applied.get('seconds')}s")
    if tests:
        timings.append(f"test {tests.get('seconds')}s")
    if timings:
        lines += ["## Time", "", "; ".join(timings) + ".", ""]
    return "\n".join(lines), data


def step_summary(ctx: Context) -> None:
    if ctx.dry_run:
        print(f"summary: {ctx.path('summary.md')} and summary.json")
        return
    ctx.bump()
    text, data = render_summary(ctx.work)
    ctx.path("summary.md").write_text(text)
    write_json(ctx.path("summary.json"), data)
    print(text)
    log(f"summary in {ctx.path('summary.md')}")


STEP_FUNCTIONS = {"fetch": step_fetch, "record": step_record, "apply": step_apply, "test": step_test,
                  "summary": step_summary}


def parse_steps(text: str | None) -> list[str]:
    if not text or text == "all":
        return list(STEPS)
    wanted = [s.strip() for s in text.split(",") if s.strip()]
    unknown = [s for s in wanted if s not in STEPS]
    if unknown:
        die(f"unknown step {', '.join(unknown)}: the steps are {', '.join(STEPS)}")
    return [s for s in STEPS if s in wanted]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--app", required=True, choices=sorted(APPS))
    ap.add_argument("--steps", help=f"comma-separated, in any order; run in the order {','.join(STEPS)} (default: all)")
    ap.add_argument("--target", help="the release to go to, instead of the triage's rule (e.g. 7.0.3.10918)")
    ap.add_argument("--releases", default="api", metavar="api|tags|FILE",
                    help="where the triage tells stable releases from pre-releases (default: api)")
    ap.add_argument("--work", type=Path, help="the work directory (default: <UNARR_CACHE or .cache>/bump/<app>)")
    ap.add_argument("--dry-run", action="store_true", help="say what each step would do; download nothing, "
                    "write nothing (the triage still updates its clone of upstream)")
    ap.add_argument("--keep-cursor", action="store_true", help="leave upstream/<app>.lock where it is")
    ap.add_argument("--baseline", action="store_true", help="also record the pinned release, to tell the noise of "
                    "any recording from what the new release changed (doubles the recording time)")
    ap.add_argument("--lists", help="record only these request lists (comma-separated directory names)")
    ap.add_argument("--rerun", type=int, default=20, help="failing tests run once more, at most this many (default 20)")
    args = ap.parse_args(argv)
    app = APPS[args.app]
    ctx = Context(app=app, work=(args.work or cache_dir() / "bump" / app.key).resolve(), dry_run=args.dry_run,
                  releases=args.releases, target=args.target, keep_cursor=args.keep_cursor, baseline=args.baseline,
                  rerun=args.rerun, lists=[s for s in (args.lists or "").split(",") if s] or None)
    steps = parse_steps(args.steps)
    if args.dry_run:
        print(f"{app.name} bump, work directory {ctx.work}; steps: {', '.join(steps)}")
    try:
        for step in steps:
            started = time.monotonic()
            STEP_FUNCTIONS[step](ctx)
            if not args.dry_run:
                log(f"step {step} done in {time.monotonic() - started:.0f}s")
    except (triage.GitError, ValueError) as err:
        die(str(err))
    return 0


if __name__ == "__main__":
    sys.exit(main())
