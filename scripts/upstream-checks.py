#!/usr/bin/env python3
"""The checks of a port that need the original running (issue #9, section 4).

`upstream/PORTING.md`, section 10, lists what a port leaves to a job without
secrets, because the port's agent never runs an original: the recordings
made again from the new original, the proof of a golden that differs by
timing, the HTTP audit, the comparison with the originals, the replays
after the new recordings. `.github/workflows/upstream-checks.yml` runs them
on a port's branch with this script, in three steps that run in different
jobs::

    scripts/upstream-checks.py plan --pr-json pr.json ...     # which PR, which checks
    scripts/upstream-checks.py run --repo W --app radarr ...  # runs the originals; no secrets
    scripts/upstream-checks.py apply --artifact DIR --repo W  # checks DIR, commits, writes the comment

``plan`` reads the PR as `gh pr view --json` gives it and the inputs of the
workflow, refuses a PR that is not a port's (`<app>/upstream-<version>` of
this repository, into `main`, open) and prints the outputs of the job.

``run`` is the job without secrets. It drives the checkout of the port's
branch (``--repo``: its pins, its fixture scripts, its unarrd), and runs the
originals only through that repository's scripts and Make targets (`scripts/<app>-fixture.sh shell`,
`scripts/recyclarr-replay.py`, `make http-audit`, `make compare-originals`,
the replays), in the order of section 10, and writes what they gave to
``--out``: the recordings at their path in the tree under `recordings/`,
the reports, and `report.json`, which says what ran and how it ended. It
writes nothing in the repository.

``apply`` is the job that may write, and reads ``--artifact`` as untrusted:
the work of a job that ran binaries nobody vetted. Every file must be one
it expects, at the path it expects, a regular file of a sane size, JSON or
text as its kind says (`checked_file` of `scripts/pin-bump.py`); anything
else stops it before it writes. What it commits it decides from its own
tables, not from the artifact: a recording whose test only compares
answers (the Recyclarr exchange) is committed, one commit per recording; a
recording whose test follows sequence numbers or ids is never committed,
since a job cannot change that test: it stays in the artifact for the port
or a session. The comparison's tables replace those of
`docs/originals-comparison.md`, with a History row and the list of routes
where Unarr is slower, in one commit. It never touches Unarr's code, the
log, the PR's table or the cursor. ``--comment`` is the report for the PR:
values it checked, and what the originals and Unarr answered inside code
blocks, as data.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REPO = SCRIPTS.parent
# The checkout whose originals `run` starts, with its own scripts and pins:
# the port's branch (--repo), which this script, from main, drives.
WORK = REPO
KEY = "0123456789abcdef0123456789abcdef"

APPS = ("sonarr", "radarr")
# The branch of a port (upstream/PORTING.md, section 1).
PORT_BRANCH = re.compile(r"^(?P<app>sonarr|radarr)/upstream-(?P<version>\d+\.\d+\.\d+\.\d+)$")
SHA = re.compile(r"^[0-9a-f]{40}$")
VERSION = re.compile(r"^\d+\.\d+\.\d+\.\d+$")

# The Recyclarr that records the exchange, as tests/fixtures/recyclarr/README.md
# names it: the release asset, its SHA-256, and the TRaSH-Guides commit.
RECYCLARR_VERSION = "8.8.1"
RECYCLARR_ASSET = "recyclarr-linux-x64.tar.xz"
RECYCLARR_SHA256 = "960fe20cf2dd1c322ca7a6395469dcbefeaa7df08b217caad7686023b840feb9"
TRASH_GUIDES = "https://github.com/TRaSH-Guides/Guides"
TRASH_GUIDES_COMMIT = "b8967a742411667577bf6cc6859fa9ae05e15549"


@dataclass(frozen=True)
class Recording:
    """A recording the job can make again. `follows_ids`: its test follows
    sequence numbers, ids or hashes of the recording, so a new one changes
    the test and is never committed by the job (requirement of issue #9:
    the port or a session changes the test)."""

    name: str
    path: str  # relative to the repository, with {app}
    follows_ids: bool
    automated: bool
    what: str


RECORDINGS = {
    "recyclarr": Recording("recyclarr", "tests/fixtures/recyclarr/{app}-exchange.json", False, True,
                           "the Recyclarr exchange, between the real Recyclarr and the original"),
    # The download clients' captures are a sequence of moments driven by
    # hand (a client tested and saved, releases pushed, one imported, one
    # removed, a ratio reached), and their test names the exchanges by
    # their sequence numbers and info hashes: a job would only ever hand
    # them on. They stay with a session (PORTING.md, section 10).
    "transmission": Recording("transmission", "tests/fixtures/downloadclients/{prefix}transmission/capture.json",
                              True, False, "the Transmission capture, between the original and the real Transmission"),
    "sabnzbd": Recording("sabnzbd", "tests/fixtures/downloadclients/{prefix}sabnzbd/capture.json",
                         True, False, "the SABnzbd capture, between the original and the real SABnzbd"),
}
LIBRARIES = ("real", "scaled")
COMPARE_CHOICES = {"none": (), "real": ("real",), "scaled": ("scaled",), "both": ("real", "scaled")}
# A request of the API asked of the old pin for a golden that differs by
# timing (PORTING.md, section 2): a path and a query, nothing else.
OLD_PIN_REQUEST = re.compile(r"^/api/v3/[A-Za-z0-9/_-]*(\?[A-Za-z0-9_=&.,%-]*)?$")
MAX_WAIT = 1800

STATUSES = {"ok", "differences", "failed", "skipped"}


class ChecksError(SystemExit):
    pass


def die(msg: str) -> None:
    raise ChecksError(f"upstream-checks: {msg}")


def log(msg: str) -> None:
    print(f"[checks] {msg}", file=sys.stderr, flush=True)


def load_script(name: str, file: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / file)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def recording_path(rec: Recording, app: str) -> str:
    return rec.path.format(app=app, prefix="" if app == "sonarr" else f"{app}-")


# --- plan -------------------------------------------------------------------


def check_port_pr(pr: dict, repository: str) -> dict:
    """A PR of this repository whose branch is a port's, open, into main.
    Returns the app, the branch, its head and the version of the branch."""
    branch = pr.get("headRefName")
    m = PORT_BRANCH.match(branch) if isinstance(branch, str) else None
    if not m:
        die(f"PR #{pr.get('number')}: its branch {branch!r} is not a port's (<app>/upstream-<version>)")
    if pr.get("state") != "OPEN":
        die(f"PR #{pr.get('number')} is not open")
    if pr.get("baseRefName") != "main":
        die(f"PR #{pr.get('number')} does not go into main")
    if pr.get("isCrossRepository"):
        die(f"PR #{pr.get('number')} comes from another repository")
    owner = (pr.get("headRepositoryOwner") or {}).get("login")
    name = (pr.get("headRepository") or {}).get("name")
    if repository and f"{owner}/{name}" != repository:
        die(f"PR #{pr.get('number')}: its branch is in {owner}/{name}, not {repository}")
    head = pr.get("headRefOid")
    if not isinstance(head, str) or not SHA.match(head):
        die(f"PR #{pr.get('number')}: no head commit")
    labels = {lab.get("name") for lab in pr.get("labels") or [] if isinstance(lab, dict)}
    return {"number": int(pr["number"]), "app": m["app"], "branch": branch, "version": m["version"],
            "head": head, "draft": bool(pr.get("isDraft")), "major": "major" in labels}


def boolean(text: str | bool) -> bool:
    if isinstance(text, bool):
        return text
    if text in ("true", "1", "yes"):
        return True
    if text in ("false", "0", "no", ""):
        return False
    die(f"not a boolean: {text!r}")


def parse_checks(recordings: str, http_audit, compare: str, replays, old_pin_request: str,
                 old_pin_wait) -> dict:
    """The inputs of the workflow, checked: what `run` does."""
    names = [n.strip() for n in (recordings or "").split(",") if n.strip() and n.strip() != "none"]
    for n in names:
        if n not in RECORDINGS:
            die(f"unknown recording {n!r}; known: {', '.join(RECORDINGS)}")
    if len(set(names)) != len(names):
        die("a recording is named twice")
    if compare not in COMPARE_CHOICES:
        die(f"compare is one of {', '.join(COMPARE_CHOICES)}, not {compare!r}")
    request = (old_pin_request or "").strip()
    if request and not OLD_PIN_REQUEST.match(request):
        die(f"old-pin request {request!r} is not a path of the API (/api/v3/...)")
    try:
        wait = int(old_pin_wait or 0)
    except ValueError:
        die(f"old-pin wait is a number of seconds, not {old_pin_wait!r}")
    if not 0 <= wait <= MAX_WAIT:
        die(f"old-pin wait is from 0 to {MAX_WAIT} seconds")
    checks = {"recordings": names, "old_pin": {"request": request, "wait": wait} if request else None,
              "http_audit": boolean(http_audit), "compare": list(COMPARE_CHOICES[compare]),
              "replays": boolean(replays)}
    if not (names or checks["old_pin"] or checks["http_audit"] or checks["compare"] or checks["replays"]):
        die("no check asked for")
    return checks


def artifact_name(pr: int, head: str) -> str:
    """The name of the results' artifact: one per PR and commit checked, so
    that two runs of the checks in one run of a calling workflow (two
    personalities, or two rounds of upstream-loop.yml on one PR at two
    commits) do not ask for the same name, which GitHub refuses."""
    return f"upstream-checks-results-{int(pr)}-{head[:12]}"


def cmd_plan(args) -> int:
    pr = check_port_pr(json.loads(Path(args.pr_json).read_text()), args.repository)
    if args.pr is not None and int(args.pr) != pr["number"]:
        die(f"asked for PR #{args.pr}, read #{pr['number']}")
    checks = parse_checks(args.recordings, args.http_audit, args.compare, args.replays,
                          args.old_pin_request, args.old_pin_wait)
    out = {"app": pr["app"], "branch": pr["branch"], "head": pr["head"], "checks": json.dumps(checks),
           "artifact": artifact_name(pr["number"], pr["head"])}
    for k, v in out.items():
        print(f"{k}={v}")
    return 0


# --- run (the job without secrets) ------------------------------------------


def cache_root() -> Path:
    return Path(os.environ.get("UNARR_CACHE") or WORK / ".cache")


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def http_get(url: str, timeout: float = 10) -> tuple[int, bytes]:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    req = urllib.request.Request(url, headers={"X-Api-Key": KEY})
    try:
        with opener.open(req, timeout=timeout) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def wait_up(url: str, seconds: float = 240) -> None:
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            if http_get(url, 5)[0] == 200:
                return
        except OSError:
            pass
        time.sleep(1)
    die(f"{url} did not answer in {seconds:.0f} s")


class Process:
    """A process group started in the background and stopped whole."""

    def __init__(self, cmd: list[str], logfile: Path, env: dict | None = None, cwd: Path | None = None):
        logfile.parent.mkdir(parents=True, exist_ok=True)
        self.log = logfile.open("ab")
        log(f"starting {' '.join(cmd)} (log {logfile})")
        self.proc = subprocess.Popen(cmd, stdout=self.log, stderr=subprocess.STDOUT, env=env, cwd=cwd or WORK,
                                     start_new_session=True)

    def stop(self) -> None:
        if self.proc.poll() is None:
            # TERM first, so the fixture script's trap stops the original
            # and the mock; the group then, for what is left.
            os.killpg(self.proc.pid, signal.SIGTERM)
            try:
                self.proc.wait(30)
            except subprocess.TimeoutExpired:
                os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait()
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.log.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.stop()


def run_logged(cmd: list[str], logfile: Path, env: dict | None = None, cwd: Path | None = None,
               timeout: float | None = None) -> int:
    logfile.parent.mkdir(parents=True, exist_ok=True)
    log(f"running {' '.join(cmd)} (log {logfile})")
    with logfile.open("ab") as f:
        try:
            return subprocess.run(cmd, stdout=f, stderr=subprocess.STDOUT, env=env, cwd=cwd or WORK,
                                  timeout=timeout).returncode
        except subprocess.TimeoutExpired:
            f.write(f"\n[checks] timed out after {timeout} s\n".encode())
            return 124


def original_port(app: str, audit: bool = False) -> int:
    """The original on its own port, or beside unarrd for the audit."""
    return {"sonarr": 8990 if audit else 8989, "radarr": 7880 if audit else 7878}[app]


def start_original(app: str, port: int, logfile: Path, repo: Path | None = None,
                   cache: Path | None = None) -> Process:
    """The pinned original of `repo`, on a copy of its fixture, through the
    fixture script (`shell`): the one way the repository runs an original."""
    repo = repo or WORK
    # No network but the loopback: the mock answers for the Servarr services.
    env = dict(os.environ, **{f"{app.upper()}_PORT": str(port)}, SHELL_VIA_MOCK="1")
    if cache is not None:
        env["UNARR_CACHE"] = str(cache)
    p = Process(["bash", str(repo / "scripts" / f"{app}-fixture.sh"), "shell"], logfile, env, repo)
    try:
        wait_up(f"http://127.0.0.1:{port}/api/v3/system/status", 300)
    except ChecksError:
        p.stop()
        raise
    return p


def start_unarrd(unarrd: Path, data: Path, logfile: Path) -> Process:
    """unarrd built from the branch, both personalities, on fresh copies of
    the fixtures, as `make run-both` starts it (Sonarr 8989, Radarr 7878)."""
    shutil.rmtree(data, ignore_errors=True)
    for app in APPS:
        (data / app).mkdir(parents=True)
        for name in ("config.xml", f"{app}.db"):
            shutil.copyfile(WORK / "tests" / "fixtures" / app / name, data / app / name)
    p = Process([str(unarrd), "--data", str(data), "--personalities", "sonarr,radarr", "--bind", "127.0.0.1"],
                logfile, dict(os.environ, RUST_LOG=os.environ.get("RUST_LOG", "info")))
    try:
        for port in (8989, 7878):
            wait_up(f"http://127.0.0.1:{port}/api/v3/system/status", 300)
    except ChecksError:
        p.stop()
        raise
    return p


def recyclarr_dir() -> Path:
    return cache_root() / "upstream-checks" / "recyclarr"


def old_pin_tree() -> Path:
    return cache_root() / "upstream-checks" / "main"


def old_pin_cache(app: str) -> Path:
    return cache_root() / "upstream-checks" / f"main-{app}"


def prepare_recyclarr(work: Path) -> Path:
    """`prepare`, with the network: Recyclarr's release, its SHA-256 checked,
    and TRaSH-Guides at its commit."""
    work.mkdir(parents=True, exist_ok=True)
    tarball = work / RECYCLARR_ASSET
    if not tarball.exists() or sha256_file(tarball) != RECYCLARR_SHA256:
        url = f"https://github.com/recyclarr/recyclarr/releases/download/v{RECYCLARR_VERSION}/{RECYCLARR_ASSET}"
        log(f"downloading {url}")
        with urllib.request.urlopen(url, timeout=120) as r, tarball.open("wb") as f:
            shutil.copyfileobj(r, f)
    got = sha256_file(tarball)
    if got != RECYCLARR_SHA256:
        die(f"{RECYCLARR_ASSET}: SHA-256 {got}, expected {RECYCLARR_SHA256}")
    bin_dir = work / "bin"
    shutil.rmtree(bin_dir, ignore_errors=True)
    bin_dir.mkdir()
    with tarfile.open(tarball) as tar:
        tar.extractall(bin_dir, filter="data")
    binary = bin_dir / "recyclarr"
    if not binary.is_file():
        die(f"no recyclarr in {RECYCLARR_ASSET}")
    binary.chmod(0o755)
    guide = work / "guide"
    if not (guide / ".git").exists():
        subprocess.run(["git", "init", "-q", str(guide)], check=True)
    subprocess.run(["git", "-C", str(guide), "fetch", "-q", "--depth", "1", TRASH_GUIDES, TRASH_GUIDES_COMMIT],
                   check=True)
    subprocess.run(["git", "-C", str(guide), "checkout", "-q", "--detach", TRASH_GUIDES_COMMIT], check=True)
    head = subprocess.run(["git", "-C", str(guide), "rev-parse", "HEAD"], capture_output=True, text=True,
                          check=True).stdout.strip()
    if head != TRASH_GUIDES_COMMIT:
        die(f"TRaSH-Guides is at {head}, not {TRASH_GUIDES_COMMIT}")
    return binary


def recyclarr_ready(work: Path) -> Path:
    """`run`, with no network: what `prepare` left, checked again."""
    tarball, binary, guide = work / RECYCLARR_ASSET, work / "bin" / "recyclarr", work / "guide"
    if not tarball.is_file() or sha256_file(tarball) != RECYCLARR_SHA256 or not binary.is_file():
        die("Recyclarr is not prepared (make upstream-checks-prepare, with the network)")
    head = subprocess.run(["git", "-C", str(guide), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    if head != TRASH_GUIDES_COMMIT:
        die("TRaSH-Guides is not prepared at its commit (make upstream-checks-prepare)")
    return binary


def record_recyclarr(app: str, out: Path, logs: Path) -> dict:
    work = recyclarr_dir()
    binary = recyclarr_ready(work)
    dest = out / "recordings" / recording_path(RECORDINGS["recyclarr"], app)
    dest.parent.mkdir(parents=True, exist_ok=True)
    port = original_port(app)
    with start_original(app, port, logs / f"original-{app}-recyclarr.log"):
        code = run_logged([sys.executable, "-I", str(WORK / "scripts" / "recyclarr-replay.py"), "--app", app,
                           "--base-url", f"http://127.0.0.1:{port}", "--api-key", KEY,
                           "--recyclarr", str(binary), "--guide", str(work / "guide"),
                           "--settle", "6", "--record", str(dest)], logs / "recyclarr-record.log", timeout=1800)
    if code != 0 or not dest.is_file():
        return {"status": "failed", "exit": code}
    tracked = WORK / recording_path(RECORDINGS["recyclarr"], app)
    same = tracked.is_file() and tracked.read_bytes() == dest.read_bytes()
    return {"status": "ok", "exit": 0, "changed": not same,
            "tool": {"recyclarr": RECYCLARR_VERSION, "sha256": RECYCLARR_SHA256, "guide": TRASH_GUIDES_COMMIT}}


def old_pin(app: str, request: str, wait: int, out: Path, logs: Path) -> dict:
    """The request a port named, asked of main's pin at once and after the
    wait, and the original's log file (PORTING.md, sections 2 and 10)."""
    tree, cache = old_pin_tree(), old_pin_cache(app)
    if not (tree / "upstream" / f"{app}.pin").is_file():
        die("main's worktree is not prepared (make upstream-checks-prepare, with the network)")
    port = original_port(app)
    dest = out / "old-pin"
    dest.mkdir(parents=True, exist_ok=True)
    with start_original(app, port, logs / f"original-{app}-old-pin.log", tree, cache):
        first = http_get(f"http://127.0.0.1:{port}{request}", 60)
        time.sleep(wait)
        later = http_get(f"http://127.0.0.1:{port}{request}", 60)
        logs_dir = cache / "golden-data" / "logs"
        text = "".join(p.read_text(errors="replace") for p in sorted(logs_dir.glob("*.txt"))) if logs_dir.is_dir() else ""
    (dest / "at-once.txt").write_bytes(f"{first[0]}\n".encode() + first[1])
    (dest / "later.txt").write_bytes(f"{later[0]}\n".encode() + later[1])
    (dest / "log.txt").write_text(text)
    pinned = read_pin(tree / "upstream" / f"{app}.pin").get("version", "")
    subprocess.run(["git", "worktree", "remove", "--force", str(tree)], cwd=WORK, capture_output=True)
    return {"status": "ok", "request": request, "wait": wait, "version": pinned,
            "statuses": [first[0], later[0]]}


def prepare_old_pin(app: str, logs: Path) -> int:
    """`prepare`, with the network: a worktree of main, and the tarball of
    its pin, downloaded and checked by its fixture script (`ui`, which runs
    nothing of it), so that `run` starts the old pin with no network."""
    tree = old_pin_tree()
    subprocess.run(["git", "worktree", "remove", "--force", str(tree)], cwd=WORK, capture_output=True)
    shutil.rmtree(tree, ignore_errors=True)
    code = run_logged(["git", "fetch", "-q", "origin", "main"], logs / "prepare-old-pin.log")
    if code == 0:
        code = run_logged(["git", "worktree", "add", "-q", "--detach", str(tree), "FETCH_HEAD"],
                          logs / "prepare-old-pin.log")
    if code == 0:
        code = run_logged(["bash", str(tree / "scripts" / f"{app}-fixture.sh"), "ui"], logs / "prepare-old-pin.log",
                          env=dict(os.environ, UNARR_CACHE=str(old_pin_cache(app))), cwd=tree)
    return code


def cmd_prepare(args) -> int:
    """What `run` needs from the network, fetched first: `run` then goes
    without it. A failure is logged, and `run` reports the check failed."""
    global WORK
    if os.environ.get("UNARR_NO_ORIGINAL"):
        die("UNARR_NO_ORIGINAL is set: this job may not run an original")
    WORK = Path(args.repo).resolve()
    checks = json.loads(args.checks)
    out = Path(args.out)
    shutil.rmtree(out, ignore_errors=True)
    logs = out / "logs"
    logs.mkdir(parents=True)
    if "recyclarr" in checks.get("recordings", []):
        try:
            prepare_recyclarr(recyclarr_dir())
        except (ChecksError, OSError, subprocess.SubprocessError) as err:
            log(f"prepare recyclarr: {err}")
    if checks.get("old_pin"):
        code = prepare_old_pin(args.app, logs)
        if code != 0:
            log(f"prepare old pin: exit {code}, see {logs / 'prepare-old-pin.log'}")
    return 0


def http_audit(app: str, unarrd: Path, out: Path, logs: Path) -> dict:
    port = original_port(app, audit=True)
    with start_original(app, port, logs / f"original-{app}-audit.log"), \
            start_unarrd(unarrd, cache_root() / "upstream-checks" / "unarr-audit", logs / "unarrd-audit.log"):
        code = run_logged(["make", "-s", "http-audit", f"ORIGINAL_URL=http://127.0.0.1:{port}", f"AUDIT_APP={app}"],
                          out / "http-audit.txt", timeout=1800)
    return audit_result(code, (out / "http-audit.txt").read_text(errors="replace"))


def audit_result(code: int, text: str) -> dict:
    """The audit's last summary line says how it ended; make exits 2 when
    the script exits 1 (something differs), and so it does on any error."""
    found = re.findall(r"^(\d+) requests, (\d+) with a difference", text, re.MULTILINE)
    if not found or (code == 0) != (found[-1][1] == "0"):
        return {"status": "failed", "exit": code}
    requests, different = map(int, found[-1])
    return {"status": "ok" if different == 0 else "differences", "exit": code,
            "requests": requests, "different": different}


def compare(libraries: list[str], out: Path, logs: Path, unarrd: Path, head: str) -> dict:
    """`make compare-originals`'s script with the unarrd the job built (the
    user that runs the originals has no cargo), named in the report by the
    commit it was built from, as a build by the script names it."""
    results = {}
    for lib in libraries:
        if lib == "scaled":
            code = run_logged(["make", "-s", "scale-fixture"], logs / "scale-fixture.log")
            if code != 0:
                results[lib] = {"status": "failed", "exit": code}
                continue
        code = run_logged([sys.executable, "-I", str(WORK / "scripts" / "compare-originals.py"), "--library", lib,
                           "--repeat", "1", "--unarrd", str(unarrd)], logs / f"compare-{lib}.log", timeout=3 * 3600)
        report = cache_root() / "compare" / lib / "report.md"
        if code != 0 or not report.is_file():
            results[lib] = {"status": "failed", "exit": code}
            continue
        dest = out / "compare" / lib
        dest.mkdir(parents=True, exist_ok=True)
        (dest / "report.md").write_text(name_the_build(report.read_text(), unarrd, head))
        results[lib] = {"status": "ok", "exit": 0}
    return results


def name_the_build(report: str, unarrd: Path, head: str) -> str:
    """The report names a binary given with --unarrd by its path: the job
    built it from `head`, so the commit takes the path's place."""
    path = str(unarrd.resolve())
    return report.replace(f"Unarr `{path}, sha256 ", f"Unarr `{head[:7]}, sha256 ", 1)


def replays(app: str, unarrd: Path, out: Path, logs: Path, exchange: Path | None) -> dict:
    """Bazarr, Prowlarr and Recyclarr against Unarr, in the order of
    PORTING.md, section 8, check 4; Recyclarr against the exchange just
    recorded when there is one."""
    url = f"http://127.0.0.1:{8989 if app == 'sonarr' else 7878}"
    suffix = "" if app == "sonarr" else f"-{app}"
    report = out / "replays.txt"
    codes = {}
    with start_unarrd(unarrd, cache_root() / "upstream-checks" / "unarr-replays", logs / "unarrd-replays.log"):
        for client in ("bazarr", "prowlarr"):
            codes[client] = run_logged(["make", "-s", f"{client}{suffix}"], report, timeout=1800)
        if exchange is not None:
            codes["recyclarr"] = run_logged([sys.executable, "-I", str(WORK / "scripts" / "recyclarr-replay.py"), "--app", app,
                                             "--base-url", url, "--api-key", KEY, "--expect", str(exchange)],
                                            report, timeout=1800)
        else:
            codes["recyclarr"] = run_logged(["make", "-s", f"recyclarr{suffix}"], report, timeout=1800)
    return {"status": "ok" if not any(codes.values()) else "differences", "exits": codes}


def read_pin(path: Path) -> dict[str, str]:
    out = {}
    for line in path.read_text().splitlines():
        m = re.match(r"^([a-z_]+)\s*=\s*(.*?)\s*$", line)
        if m:
            out[m[1]] = m[2]
    return out


def cmd_run(args) -> int:
    global WORK
    if os.environ.get("UNARR_NO_ORIGINAL"):
        die("UNARR_NO_ORIGINAL is set: this job may not run an original")
    WORK = Path(args.repo).resolve()
    checks = json.loads(args.checks)
    app = args.app
    if app not in APPS:
        die(f"unknown app {app!r}")
    unarrd = Path(args.unarrd) if args.unarrd else WORK / "target" / "release" / "unarrd"
    out, logs = Path(args.out), Path(args.out) / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=WORK, capture_output=True, text=True).stdout.strip()
    pin = read_pin(WORK / "upstream" / f"{app}.pin")
    report = {"app": app, "head": head, "version": pin.get("version", ""), "checks": {}}
    started = time.monotonic()

    def step(name: str, fn):
        t = time.monotonic()
        try:
            result = fn()
        except (ChecksError, OSError, subprocess.SubprocessError) as err:
            log(f"{name}: {err}")
            result = {"status": "failed", "error": str(err)[:300]}
        result["seconds"] = round(time.monotonic() - t)
        report["checks"][name] = result
        (out / "report.json").write_text(json.dumps(report, indent=1, sort_keys=True) + "\n")

    exchange = None
    for name in checks.get("recordings", []):
        rec = RECORDINGS[name]
        if not rec.automated:
            report["checks"][f"record:{name}"] = {"status": "skipped", "seconds": 0}
            continue
        step(f"record:{name}", lambda: record_recyclarr(app, out, logs))
        if report["checks"][f"record:{name}"]["status"] == "ok":
            exchange = out / "recordings" / recording_path(rec, app)
    if checks.get("old_pin"):
        step("old-pin", lambda: old_pin(app, checks["old_pin"]["request"], checks["old_pin"]["wait"], out, logs))
    if checks.get("http_audit"):
        step("http-audit", lambda: http_audit(app, unarrd, out, logs))
    for lib in checks.get("compare", []):
        step(f"compare:{lib}", lambda lib=lib: compare([lib], out, logs, unarrd, head)[lib])
    if checks.get("replays"):
        step("replays", lambda: replays(app, unarrd, out, logs, exchange))
    report["seconds"] = round(time.monotonic() - started)
    (out / "report.json").write_text(json.dumps(report, indent=1, sort_keys=True) + "\n")
    log(f"done in {report['seconds']} s; {out / 'report.json'}")
    return 0


# --- main is protected from the workflows' tokens -----------------------------

# A token that may write contents can push to main, and merge a PR, through
# the API, unless a ruleset stops it. The workflows that write (the port's
# agent, apply) start only when main has the rule "Restrict updates" (`update`:
# only who is in the ruleset's bypass list updates the branch) in an active
# ruleset that this token cannot bypass. The maintainer, in the bypass list,
# merges as before; no approval is asked of anyone (decided 2026-11-24).


def protection_problems(rules: list, rulesets: dict) -> list[str]:
    """Why main is not protected from the token that read `rulesets`
    (`current_user_can_bypass` is the reader's own): an empty list when it is."""
    if not isinstance(rules, list):
        return ["the rules of main could not be read"]
    found = []
    for rule in rules:
        if not isinstance(rule, dict) or rule.get("type") != "update":
            continue
        ruleset = rulesets.get(rule.get("ruleset_id"))
        if not isinstance(ruleset, dict):
            found.append(f"the ruleset {rule.get('ruleset_id')!r} of the rule \"Restrict updates\" could not be read")
            continue
        bypass = ruleset.get("current_user_can_bypass")
        if bypass != "never":
            found.append(f"the ruleset {ruleset.get('name')!r}: this token can bypass it ({bypass!r})")
            continue
        return []
    return found or ["no active ruleset on main has the rule \"Restrict updates\""]


def github_get(path: str, token: str) -> object:
    api = os.environ.get("GITHUB_API_URL", "https://api.github.com")
    req = urllib.request.Request(f"{api}{path}", headers={
        "Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read())


PROTECT_HINT = ("Add a branch ruleset on main with \"Restrict updates\", active, with only the Repository admin "
                "role in its bypass list (upstream/README.md, \"What the maintainer sets\").")


def main_protection(repository: str, token: str) -> list[str]:
    """Why main is not protected from `token`, asked of GitHub: an empty
    list when it is. Stops when the rules cannot be read."""
    if not re.match(r"^[\w.-]+/[\w.-]+$", repository) or not token:
        die("GITHUB_REPOSITORY and GH_TOKEN are needed")
    try:
        rules = github_get(f"/repos/{repository}/rules/branches/main", token)
        ids = {r.get("ruleset_id") for r in rules if isinstance(r, dict) and r.get("type") == "update"} \
            if isinstance(rules, list) else set()
        rulesets = {i: github_get(f"/repos/{repository}/rulesets/{int(i)}", token)
                    for i in ids if isinstance(i, int)}
    except (OSError, ValueError) as err:
        die(f"the rules of main could not be read: {err}")
    return protection_problems(rules, rulesets)


def cmd_main_protected(args) -> int:
    problems = main_protection(os.environ.get("GITHUB_REPOSITORY", ""),
                               os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN") or "")
    if problems:
        print("main is not protected from this workflow's token, which may write: "
              + "; ".join(problems) + ". " + PROTECT_HINT, file=sys.stderr)
        return 1
    print("main restricts updates to its bypass list, and this token cannot bypass it")
    return 0


# --- apply (the job that may write) -----------------------------------------

MAX_JSON = 32 * 1024 * 1024
MAX_TEXT = 2 * 1024 * 1024
MAX_REPORT = 64 * 1024
MAX_COMMENT = 60000


def expected_files(app: str) -> dict[str, tuple[str, int]]:
    """Every path the artifact may hold: kind and largest size."""
    files = {"report.json": ("json", 1024 * 1024),
             "http-audit.txt": ("text", MAX_TEXT),
             "replays.txt": ("text", MAX_TEXT),
             "old-pin/at-once.txt": ("text", MAX_TEXT),
             "old-pin/later.txt": ("text", MAX_TEXT),
             "old-pin/log.txt": ("text", MAX_TEXT)}
    for rec in RECORDINGS.values():
        files[f"recordings/{recording_path(rec, app)}"] = ("json", MAX_JSON)
    for lib in LIBRARIES:
        files[f"compare/{lib}/report.md"] = ("text", MAX_REPORT)
    return files


def checked_artifact(root: Path, app: str) -> dict[str, bytes]:
    """Every file of the artifact, read as untrusted. A link, a path not
    expected, a file too large, or JSON that does not parse stops it all."""
    bump = load_script("pin_bump_for_checks", "pin-bump.py")
    expected = expected_files(app)
    files = {}
    if root.is_symlink() or not root.is_dir():
        die(f"{root} is not a directory")
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root).as_posix()
        if path.is_symlink():
            die(f"{rel}: a link")
        if path.is_dir():
            if not any(e.startswith(rel + "/") for e in expected):
                die(f"{rel}: a directory not expected")
            continue
        if rel not in expected:
            die(f"{rel}: a file not expected")
        kind, limit = expected[rel]
        if path.stat().st_size > limit:
            die(f"{rel}: larger than {limit} bytes")
        try:
            raw = bump.checked_file(path, kind == "json")
        except SystemExit as err:
            die(str(err))
        if kind == "text":
            try:
                raw.decode("utf-8")
            except UnicodeDecodeError:
                die(f"{rel}: not UTF-8")
        files[rel] = raw
    if "report.json" not in files:
        die("no report.json")
    return files


def check_report(report: dict, app: str, head: str) -> dict:
    """The fields of report.json the comment prints, each checked."""
    if not isinstance(report, dict) or report.get("app") != app:
        die("report.json is not for this app")
    if report.get("head") != head:
        die(f"report.json was made at {report.get('head')!r}, not at {head}")
    if not VERSION.match(str(report.get("version", ""))):
        die("report.json: no version")
    checks = report.get("checks")
    if not isinstance(checks, dict):
        die("report.json: no checks")
    names = {f"record:{n}" for n in RECORDINGS} | {"old-pin", "http-audit", "replays"} | \
        {f"compare:{lib}" for lib in LIBRARIES}
    clean = {}
    for name, result in checks.items():
        if name not in names or not isinstance(result, dict) or result.get("status") not in STATUSES:
            die(f"report.json: check {name!r} not understood")
        c = {"status": result["status"]}
        for key in ("exit", "seconds", "requests", "different", "wait"):
            if key in result:
                if not isinstance(result[key], int) or isinstance(result[key], bool):
                    die(f"report.json: {name}.{key} is not a number")
                c[key] = result[key]
        if "changed" in result:
            if not isinstance(result["changed"], bool):
                die(f"report.json: {name}.changed")
            c["changed"] = result["changed"]
        if "exits" in result:
            exits = result["exits"]
            if not isinstance(exits, dict) or set(exits) - {"bazarr", "prowlarr", "recyclarr"} or \
                    not all(isinstance(v, int) and not isinstance(v, bool) for v in exits.values()):
                die(f"report.json: {name}.exits")
            c["exits"] = exits
        if "request" in result:
            if not isinstance(result["request"], str) or not OLD_PIN_REQUEST.match(result["request"]):
                die(f"report.json: {name}.request")
            c["request"] = result["request"]
        if "statuses" in result:
            st = result["statuses"]
            if not isinstance(st, list) or not all(isinstance(v, int) and not isinstance(v, bool) for v in st):
                die(f"report.json: {name}.statuses")
            c["statuses"] = st
        clean[name] = c
    return {"version": report["version"], "checks": clean}


# The comparison's report.md, as scripts/compare-originals.py writes it.
LIB_HEADING = {"real": "### Real library", "scaled": "### Scaled library"}
CELL = r"(?:n/a|\d+(?:\.\d+)? (?:s|MiB|s/h)(?: \(\d+(?:\.\d+)?–\d+(?:\.\d+)?\))?)"
LESS = r"(?:n/a|\d+ %(?: more)?|\d+(?:\.\d+)?× (?:sooner|slower))"
LAT = r"(?:n/a|\d+(?:\.\d+)?(?: \(\d+(?:\.\d+)?–\d+(?:\.\d+)?\))? / \d+(?:\.\d+)?)"
SUMMARY_LABELS = [
    "First answer on both ports", "Idle, at the end of the idle window: RSS", "Same, anonymous memory",
    "Idle CPU, from minute 5 of the window", "The reads: CPU", "The reads: time",
    "Peak anonymous memory during the reads", "Anonymous memory at the end of the tail",
    "Idle CPU in the tail, from its first minute",
]
META = re.compile(r"^Library `(?P<lib>real|scaled)`; Unarr `(?P<unarr>[0-9a-f]{7,12})(?:[^`]*)`; "
                  r"[A-Za-z0-9 .,;:`*()'+/_\-–×%]*?(?P<date>\d{4}-\d{2}-\d{2})\.$")
SAFE_META = re.compile(r"^[A-Za-z0-9 .,;:`*()'+/_\-–×%]+$")


def parse_report(text: str, library: str) -> dict:
    """report.md checked line by line: the shape compare-originals.py writes,
    each value of the form it gives. Anything else stops the update."""
    lines = text.rstrip("\n").split("\n")
    m = META.match(lines[0]) if lines else None
    if not m or not SAFE_META.match(lines[0]) or m["lib"] != library:
        die(f"report.md of {library}: its first line is not the measurement's")
    expect = ["", "| | Sonarr + Radarr | Unarr | Less |", "|---|--:|--:|--:|"]
    if lines[1:4] != expect:
        die(f"report.md of {library}: the summary table's header")
    summary = {}
    for i, label in enumerate(SUMMARY_LABELS):
        row = lines[4 + i] if 4 + i < len(lines) else ""
        rm = re.fullmatch(rf"\| {re.escape(label)} \| ({CELL}) \| ({CELL}) \| ({LESS}) \|", row)
        if not rm:
            die(f"report.md of {library}: row {label!r}")
        summary[label] = (rm[1], rm[2], rm[3])
    rest = lines[4 + len(SUMMARY_LABELS):]
    if rest[:3] != ["", "| p50 / max, ms | Originals | Unarr |", "|---|--:|--:|"]:
        die(f"report.md of {library}: the latency table's header")
    latency = []
    for row in rest[3:]:
        rm = re.fullmatch(rf"\| ((?:Sonarr|Radarr) /api/v3/[a-z/]+) \| ({LAT}) \| ({LAT}) \|", row)
        if not rm:
            die(f"report.md of {library}: latency row {row[:60]!r}")
        latency.append((rm[1], rm[2], rm[3]))
    if not latency:
        die(f"report.md of {library}: no latency")
    runs = re.search(r"(\d+) runs per side", lines[0])
    return {"text": "\n".join(lines) + "\n", "library": library, "unarr": m["unarr"], "date": m["date"],
            "runs": int(runs[1]) if runs else 1, "warmup": "warm-up not settled" in lines[0],
            "summary": summary, "latency": latency}


def median_of(cell: str) -> float | None:
    m = re.match(r"^(\d+(?:\.\d+)?)", cell)
    return float(m[1]) if m else None


def lat_values(cell: str) -> tuple[float, float] | None:
    m = re.match(r"^(\d+(?:\.\d+)?)(?: \([^)]*\))? / (\d+(?:\.\d+)?)$", cell)
    return (float(m[1]), float(m[2])) if m else None


def route_name(route: str) -> str:
    app, path = route.split(" ", 1)
    return f"{app}'s `{path.removeprefix('/api/v3')}`"


def slower(report: dict) -> tuple[list[str], str | None]:
    """The routes where Unarr is slower, at p50 or at the maximum; and, when
    none is, the one that comes closest at p50."""
    found, closest, ratio = [], None, 0.0
    for route, orig, unarr in report["latency"]:
        o, u = lat_values(orig), lat_values(unarr)
        if o is None or u is None:
            continue
        if u[0] > o[0]:
            found.append(f"{route_name(route)} at p50 ({u[0]:.1f} ms against {o[0]:.1f})")
        if u[1] > o[1]:
            found.append(f"{route_name(route)} at the maximum ({u[1]:.1f} ms against {o[1]:.1f})")
        if o[0] > 0 and u[0] / o[0] > ratio:
            ratio, closest = u[0] / o[0], f"{route_name(route)} comes closest, {u[0]:.1f} ms at p50 against {o[0]:.1f}"
    return found, closest


def slower_sentence(report: dict) -> str:
    found, closest = slower(report)
    lib = report["library"]
    if found:
        return f"On the {lib} library, measured at `{report['unarr']}`: " + "; ".join(found) + "."
    return f"On the {lib} library, none, at p50 or at the maximum" + (f": {closest}." if closest else ".")


def update_comparison(doc: str, report: dict, note: str) -> str:
    """docs/originals-comparison.md with the library's tables replaced by the
    report's, its paragraph of slower routes rewritten, and a History row."""
    lines = doc.split("\n")
    heading = LIB_HEADING[report["library"]]
    starts = [i for i, line in enumerate(lines) if line.startswith(heading + " ")]
    if len(starts) != 1:
        die(f"docs/originals-comparison.md: {heading} found {len(starts)} times")
    start = starts[0] + 1
    end = start
    while end < len(lines) and not lines[end].startswith("#") and not lines[end].startswith("Where Unarr is slower"):
        end += 1
    if end == len(lines):
        die("docs/originals-comparison.md: the end of the library's tables")
    new = ["", *report["text"].rstrip("\n").split("\n"), ""]
    lines[start:end] = new
    # The paragraph of slower routes for this library.
    opening = f"On the {report['library']} library"
    paras = [i for i, line in enumerate(lines) if line.startswith(opening)]
    if len(paras) != 1:
        die(f"docs/originals-comparison.md: the paragraph {opening!r} found {len(paras)} times")
    p_end = paras[0]
    while p_end < len(lines) and lines[p_end].strip():
        p_end += 1
    lines[paras[0]:p_end] = wrap(slower_sentence(report))
    # The History row, first under the table's header.
    hist = [i for i, line in enumerate(lines) if line == "## History"]
    if len(hist) != 1:
        die("docs/originals-comparison.md: no single History section")
    sep = next((i for i in range(hist[0], len(lines)) if lines[i].startswith("|---|")), None)
    if sep is None:
        die("docs/originals-comparison.md: no History table")
    s = report["summary"]
    rss, cpu = s["Idle, at the end of the idle window: RSS"], s["Idle CPU, from minute 5 of the window"]
    row = (f"| {report['date']} | {report['library']} | `{report['unarr']}` | {report['runs']} | "
           f"{strip_range(rss[0])} / {strip_range(cpu[0])} | {strip_range(rss[1])} / {strip_range(cpu[1])} | {note} |")
    lines.insert(sep + 1, row)
    return "\n".join(lines)


def strip_range(cell: str) -> str:
    return re.sub(r" \([^)]*\)", "", cell)


def wrap(text: str, width: int = 72) -> list[str]:
    out, line = [], ""
    for word in text.split(" "):
        if line and len(line) + 1 + len(word) > width:
            out.append(line)
            line = word
        else:
            line = f"{line} {word}" if line else word
    return out + [line] if line else out


def history_note(report: dict, app: str, version: str, pr: int) -> str:
    found, _ = slower(report)
    parts = [f"measured by the upstream-checks workflow on #{pr}, {app.capitalize()} {version} pinned"]
    if report["warmup"]:
        parts.append("warm-up flagged")
    parts.append(f"{len(found)} slower than the original's" if found
                 else "no route slower than the original's, at p50 or at the maximum")
    return "; ".join(parts)


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=check)


def commit(repo: Path, paths: list[str], subject: str, body: str) -> str:
    git(repo, "add", "--", *paths)
    git(repo, "commit", "-q", "-m", subject, "-m", body)
    return git(repo, "rev-parse", "HEAD").stdout.strip()


def fence(text: str, lines: int = 80, chars: int = 8000) -> str:
    """Text the originals or Unarr wrote, as data: in a code block that it
    cannot close, short enough for a comment."""
    clean = "".join(ch if ch in "\n\t" or ord(ch) >= 32 else " " for ch in text).replace("`", "'")
    kept = clean.strip("\n").split("\n")
    cut = len(kept) > lines
    body = "\n".join(kept[:lines])[:chars]
    return "```text\n" + body + ("\n... (cut: the whole file is in the artifact)" if cut else "") + "\n```"


def cmd_apply(args) -> int:
    app, repo, pr = args.app, Path(args.repo), int(args.pr)
    if app not in APPS:
        die(f"unknown app {app!r}")
    if not SHA.match(args.head):
        die("--head is not a commit")
    files = checked_artifact(Path(args.artifact), app)
    report = check_report(json.loads(files["report.json"]), app, args.head)
    if git(repo, "rev-parse", "HEAD").stdout.strip() != args.head:
        die(f"{repo} is not at {args.head}")
    version = read_pin(repo / "upstream" / f"{app}.pin").get("version", "")
    if version != report["version"]:
        die(f"the branch pins {version!r}, the checks ran {report['version']!r}")
    App = app.capitalize()
    run_url = args.run_url if re.match(r"^https://github\.com/[\w.-]+/[\w.-]+/actions/runs/\d+(/attempts/\d+)?$",
                                       args.run_url or "") else ""
    via = f"the upstream-checks workflow ({run_url})" if run_url else "the upstream-checks workflow"
    rows, details, commits, kept = [], [], [], []
    checks = report["checks"]
    for name, rec in RECORDINGS.items():
        c = checks.get(f"record:{name}")
        if c is None:
            continue
        rel = recording_path(rec, app)
        raw = files.get(f"recordings/{rel}")
        if c["status"] == "skipped" or not rec.automated:
            rows.append((f"{rec.what}, recorded again", "", "not automated: left to a session (PORTING.md, section 10)"))
            continue
        if c["status"] != "ok" or raw is None:
            rows.append((f"{rec.what}, recorded again", "", f"failed (exit {c.get('exit', '?')}): see the logs"))
            continue
        target = repo / rel
        if target.is_file() and target.read_bytes() == raw:
            rows.append((f"{rec.what}, recorded again", "", "the same as the one committed: nothing to commit"))
            continue
        if rec.follows_ids:
            kept.append(rel)
            rows.append((f"{rec.what}, recorded again", "",
                         "changed; not committed: its test follows the recording's ids, for the port to change"))
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(raw)
        sha = commit(repo, [rel], f"Record {App}'s Recyclarr exchange again from {App} {version}",
                     f"{rel} recorded again by {via}, between the real Recyclarr {RECYCLARR_VERSION} "
                     f"(the {RECYCLARR_ASSET} release, SHA-256 {RECYCLARR_SHA256}) and the original {App} "
                     f"{version} on the fixture (scripts/{app}-fixture.sh shell), with TRaSH-Guides at "
                     f"{TRASH_GUIDES_COMMIT}, as tests/fixtures/recyclarr/README.md says. The README's prose "
                     "on the releases used is the port's to update.")
        commits.append(sha)
        rows.append((f"{rec.what}, recorded again", sha[:10], "changed and committed"))
    if "old-pin" in checks:
        c = checks["old-pin"]
        rows.append((f"`{c.get('request', '?')}` asked of main's pin (a golden that differs by timing)", "",
                     f"{c['status']}; statuses {c.get('statuses', [])}, at once and after {c.get('wait', '?')} s"))
        for part in ("at-once", "later", "log"):
            if f"old-pin/{part}.txt" in files:
                details.append((f"Old pin, {part}", files[f"old-pin/{part}.txt"].decode()))
    if "http-audit" in checks:
        c = checks["http-audit"]
        text = {"ok": f"{c.get('requests')} requests, no difference",
                "differences": f"{c.get('requests')} requests, {c.get('different')} with a difference"}
        rows.append(("`make http-audit`", "", text.get(c["status"], f"failed (exit {c.get('exit', '?')})")))
        if "http-audit.txt" in files:
            details.append(("HTTP audit", files["http-audit.txt"].decode()))
    reports = []
    for lib in LIBRARIES:
        c = checks.get(f"compare:{lib}")
        if c is None:
            continue
        raw = files.get(f"compare/{lib}/report.md")
        if c["status"] != "ok" or raw is None:
            rows.append((f"`make compare-originals`, {lib} library", "", f"failed (exit {c.get('exit', '?')})"))
            continue
        reports.append(parse_report(raw.decode(), lib))
    if reports:
        doc_path = repo / "docs" / "originals-comparison.md"
        doc = doc_path.read_text()
        for r in reports:
            doc = update_comparison(doc, r, history_note(r, app, version, pr))
        doc_path.write_text(doc)
        libs = " and ".join(r["library"] for r in reports)
        sha = commit(repo, ["docs/originals-comparison.md"],
                     f"Measure the originals again on the {libs} librar{'ies' if len(reports) > 1 else 'y'} "
                     f"with {App} {version}",
                     f"make compare-originals by {via}, on the branch at {args.head[:12]}: the tables of "
                     f"the {libs} library replaced by the run's report, its History row added, and the routes "
                     "where Unarr is slower listed from the tables.")
        commits.append(sha)
        for r in reports:
            found, _ = slower(r)
            rows.append((f"`make compare-originals`, {r['library']} library", sha[:10],
                         f"Unarr `{r['unarr']}`, {r['runs']} run(s); "
                         + (f"{len(found)} slower than the original's" if found else "no route slower")))
    if "replays" in checks:
        c = checks["replays"]
        exits = ", ".join(f"{k} {'passes' if v == 0 else f'fails (exit {v})'}" for k, v in c.get("exits", {}).items())
        rows.append(("the replays against Unarr", "", exits or f"failed (exit {c.get('exit', '?')})"))
        if "replays.txt" in files and c["status"] != "ok":
            details.append(("Replays", files["replays.txt"].decode()))
    result = {"commits": commits, "kept": kept, "rows": rows, "details": details}
    Path(args.comment).write_text(render_comment(result, app, version, args.head, run_url, int(args.pr)))
    print(json.dumps({"commits": commits, "kept": kept}))
    return 0


def render_comment(result: dict, app: str, version: str, head: str, run_url: str, pr: int = 0) -> str:
    artifact = artifact_name(pr, head) if pr else "upstream-checks-results"
    out = [f"### Checks with the original {app.capitalize()} {version} running",
           "",
           f"Made by the upstream-checks workflow on `{head[:12]}`"
           + (f", [run]({run_url})" if run_url else "") + ", in the order of `upstream/PORTING.md`, section 10. "
           "It decides none of the differences: the port's next run settles them (section 9).",
           "",
           "| Check | Commit | Outcome |",
           "|---|---|---|"]
    out += [f"| {what} | {sha or ''} | {outcome} |" for what, sha, outcome in result["rows"]]
    if result["kept"]:
        out += ["", f"Recorded again but not committed, in the run's artifact `{artifact}`: "
                + ", ".join(f"`{k}`" for k in result["kept"]) + "."]
    if result["commits"]:
        out += ["", "Pushed with the job's own token, the commits start no CI by themselves: the next push of a "
                "person to the branch runs it."]
    for title, text in result["details"]:
        block = ["", f"<details><summary>{title}</summary>", "", fence(text), "", "</details>"]
        # A comment holds 65536 characters at most: what does not fit is
        # in the artifact.
        if len("\n".join(out + block)) > MAX_COMMENT:
            out += ["", f"{title}: in the artifact `{artifact}` (too long for this comment)."]
            continue
        out += block
    return "\n".join(out) + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("plan", help="check the PR and the inputs; print the job's outputs")
    p.add_argument("--pr-json", required=True)
    p.add_argument("--pr", type=int)
    p.add_argument("--repository", default="", help="owner/name the PR's branch must be in")
    p.add_argument("--recordings", default="")
    p.add_argument("--http-audit", default="false")
    p.add_argument("--compare", default="none")
    p.add_argument("--replays", default="false")
    p.add_argument("--old-pin-request", default="")
    p.add_argument("--old-pin-wait", default="0")
    q = sub.add_parser("prepare", help="what the checks need from the network, before `run` goes without it")
    q.add_argument("--repo", default=str(REPO), help="the checkout of the port's branch (default: this one)")
    q.add_argument("--app", required=True)
    q.add_argument("--checks", required=True, help="the JSON plan printed")
    q.add_argument("--out", required=True)
    r = sub.add_parser("run", help="the job without secrets: run the checks")
    r.add_argument("--app", required=True)
    r.add_argument("--checks", required=True, help="the JSON plan printed")
    r.add_argument("--out", required=True)
    r.add_argument("--repo", default=str(REPO), help="the checkout of the port's branch (default: this one)")
    r.add_argument("--unarrd", help="default: target/release/unarrd of --repo")
    a = sub.add_parser("apply", help="check the artifact, commit what may be committed, write the comment")
    a.add_argument("--artifact", required=True)
    a.add_argument("--repo", required=True, help="the checkout of the port's branch")
    a.add_argument("--app", required=True)
    a.add_argument("--pr", required=True)
    a.add_argument("--head", required=True, help="the commit the checks ran at")
    a.add_argument("--comment", required=True, help="where to write the PR comment")
    a.add_argument("--run-url", default="")
    sub.add_parser("main-protected", help="refuse unless main restricts updates to a bypass list this token "
                   "(GH_TOKEN) is not in")
    args = ap.parse_args(argv)
    return {"plan": cmd_plan, "prepare": cmd_prepare, "run": cmd_run, "apply": cmd_apply,
            "main-protected": cmd_main_protected}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
