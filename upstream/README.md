# Following the originals upstream

How Unarr follows the Sonarr and Radarr releases (issue #9). Per
personality:

| File | What | Written by |
|------|------|------------|
| `<app>.pin` | the release Unarr emulates: version, build time, migration level, SHA-256 of the tarball; the one place a pinned value is written (`scripts/tests/test_pins.py`) | `make bump-<app>` |
| `<app>.lock` | the cursor: the last upstream commit handled, and its release | `make bump-<app> STEPS=cursor`, the last step of a port |
| `<app>-wontport.md` | upstream changes Unarr does not take, so the triage does not assess them again | by hand |
| `<app>-log.csv` | one row per upstream commit a port handled: its class, the decision, the Unarr commits, the tests, the issue of what is deferred (of the whole commit, or of a part), what a human changed in review (`scripts/tests/test_port_log.py`) | the port, at the end of each run: the decisions made final in it |
| `PORTING.md` | how a port is done, for both personalities: where it starts, the order, the decisions, the PR | by hand |

Two commands, run in this order:

- `make upstream-<app>` (`scripts/upstream-triage.py`): which stable release
  to go to, and a table of the upstream commits since the cursor, classified
  by the paths they touch. It changes nothing.
- `make bump-<app>` (`scripts/pin-bump.py`): brings the tree to that release
  and says what changed. Its summary, with the triage's table, is the
  port's material.

Then the port: [`PORTING.md`](PORTING.md) is where it starts, and says how
it goes from there to the PR.

The workflow `upstream` runs both every Monday and on request, opens
the draft PR the port starts from, and then runs the port's agent on it
and the checks with the original when the port asks for them: see
[The workflow: up to the draft PR](#the-workflow-up-to-the-draft-pr) and
[The loop: the agent and the checks](#the-loop-the-agent-and-the-checks).

## `make bump-<app>`

```sh
make bump-radarr DRY_RUN=1                               # what it would do
make bump-radarr                                         # all the steps, target by the triage's rule
make bump-radarr TARGET=7.0.3.10918 RELEASES=tags        # a target given by hand, the API out of reach
make bump-radarr STEPS=fetch,record                      # only the steps that run the new original
make bump-radarr STEPS=apply,test,summary                # the rest, from the same work directory
make bump-radarr STEPS=cursor                            # at the end of the port: the cursor to the pinned release
```

Variables: `TARGET`, `RELEASES` (`api`, `tags` or a file, as for the
triage), `STEPS`, `DRY_RUN=1`, `BASELINE=1` (also record the pinned release,
to tell what any recording changes from what the release changes; twice the
recording time), `LISTS` (record only these request lists). The work directory is `.cache/bump/<app>`
(`UNARR_CACHE/bump/<app>` when `UNARR_CACHE` is set); the summary is
`summary.md` there, and `summary.json` for a program.

| Step | What it does | Writes | Network | Runs the new original |
|------|--------------|--------|---------|------------------------|
| `fetch` | the target from the triage or `TARGET`; the tarball from `https://github.com/<owner>/<repo>/releases/download/v<version>/`, the final host checked to be GitHub's release storage, its entries checked (all under `<App>/`, no link out), its SHA-256 computed and, when the releases came from the API, compared with the digest GitHub publishes | the work directory | GitHub | no |
| `record` | the fixture script (`scripts/<app>-fixture.sh golden`) on the tarball of the work directory: once with no request, for the migration level of the fixture the original migrates (that database is kept, for the importer's test); then every request list under `tests/fixtures/<app>/golden*/` with the variables of its header; the provider schemas and the localisation file; `db-after.json` and `config-after.xml`, which the headers say were copied by hand; the requests the mock got | the work directory, and the fixture script's `/movies`, `/tv`, `/downloads` | none | yes |
| `apply` | checks first, writes after. Checks: the recordings compared with `HEAD`; the triage's clone fetched and the vectors of the whole new tag made apart (`scripts/port-vectors.py`); the old binary removed from the cache and the web UI copied by the fixture script from a tarball it downloads again and checks against the fetched SHA-256. Writes: the recordings, the pin (not the cursor), the vectors (files gone upstream removed). The importer's `MAX_KNOWN_MIGRATION` is read | the repository | GitHub | no |
| `test` | `cargo test --workspace --no-fail-fast` and `make test-scripts`; each failing test run once more (20 at most), to tell a flaky one | the work directory | none | no |
| `summary` | the release, which recordings changed and how (by list and by area), the schemas and strings, the vectors, the importer, the failing tests and what each printed first, what is left to check by hand, the time each step took | the work directory | none | no |
| `cursor` | not a default step: once every upstream commit up to the pinned release is handled, the cursor moves to that release and the commit of its tag, read from the triage's clone | the repository | GitHub | no |

`record` is the only step that runs a binary nobody vetted, and it writes
nothing in the repository: in the workflow it runs in a job without
secrets, which hands the work directory on as an artifact. `apply` reads
that directory as untrusted: it copies only the files the request lists
name, refuses links, files over 64 MiB and JSON that does not parse, checks
the values it pins, and never runs the original. It downloads the tarball
again and checks it against the checksum `fetch` computed before it writes
any tracked file: a work directory changed by the original stops it there,
with the repository as it was. `summary` checks the shape of every value it prints
from the files of that job (the release URL, the host, versions, tags,
checksums, numbers, list names) and stops on one that is not right, so a
link or a mention the original wrote cannot reach a pull request that
quotes the summary.

What the bump leaves to the port, and lists in the summary:

- the importer's `MAX_KNOWN_MIGRATION`, when the new original migrates
  further: `the_pinned_release_is_within_the_known_migrations` fails until
  the mapping has been read against the new migrations, then it is raised
  and a test imports a database at the new level (the fixture the new
  original migrated is in the work directory);
- the Recyclarr exchanges (`tests/fixtures/recyclarr/`), recorded between
  the real Recyclarr and the real original;
- the metadata answers (`tests/fixtures/metadata/`): the summary lists the
  requests the mock had no answer for (`RECORD_METADATA=1` records them).
  The Servarr services' answers (`services.sonarr.tv`,
  `radarr.servarr.com`: the update check, the recent releases
  `GET /api/v3/update` lists, the notifications) are named without the
  query that carries the original's version, so the new original gets the
  answers recorded once for the old one and the bump never waits on those
  hosts; a refresh, when they change, is by hand (below). One refresh is
  due at every bump to a release newer than that recording: the original
  marks installed only the release of the list with its own version, so
  when `v1/update/<branch>/changes.json` does not list the new release,
  the recorded `update.json` has no installed release and the contract
  tests fail ("the original's update list has no installed release"); the
  summary says so and gives the command;
- the Bazarr and Prowlarr replays, written from those projects' source;
- the prose that names the old release;
- `docs/originals-comparison.md`, which a job of the workflow measures on
  the PR's branch (issue #9; below, "The checks with the original"); until
  it has, the port lists it as left to check by hand and its PR is not
  merged without it (`PORTING.md`, section 8).

Of these, what needs an original running (the recordings made again, the
metadata answers, `scripts/http-audit.py`, the comparison) the port's
agent never does: the workflow's job without secrets does it on the
port's branch and reports the differences, and a session of its own what
the job does not do (`PORTING.md`, section 10; below).

### The recording reaches no host outside the machine

When they record (`golden`), the fixture scripts start the original with
`scripts/mock-servarr.py` as its proxy from its first request (`HTTPS_PROXY`, `HTTP_PROXY`; the
loopback and `*.invalid` direct) and the mock's certificate as the only
one it trusts (`SSL_CERT_FILE`). The mock answers for the metadata hosts
and the Servarr services from `tests/fixtures/metadata/`, `v1/time` with
its own clock, and refuses any other host at once (`plex.tv`, which the
Plex connection asks, is one); the script prints the hosts it refused. So
`record` needs no network, and a Servarr host that does not answer no
longer stops a bump: on 2026-11-22 `radarr.servarr.com` did not, and
`GET /api/v3/update` waited past the recorder's 60 seconds. The original's
settings are not touched, so the goldens that show them stay as recorded.
`make sonarr` and `make radarr`, for comparing by hand on real data, leave
the original its direct way out, as before.

What the mock cannot see is a change in those hosts' answers: the metadata
servers' check (issue #9, section 7) asks them. To refresh an answer of
the Servarr services, by hand and with the hosts reachable, remove its
file and record with `RECORD_METADATA=1`, which fetches only what is
missing; for example, the recent releases of both originals, recorded
aside so that only the answers land in the tree:

```sh
rm tests/fixtures/metadata/radarr.servarr.com/v1/update/master/changes.json \
   tests/fixtures/metadata/services.sonarr.tv/v1/update/main/changes.json
RECORD_METADATA=1 GOLDEN_OUT=.cache/refresh/radarr make golden-radarr
RECORD_METADATA=1 GOLDEN_OUT=.cache/refresh/sonarr make golden
```

A refreshed answer can change goldens: `update.json` of the main lists
shows those releases, so copy it from `.cache/refresh/<app>/` and commit
it with the answer. When a host does not answer, the file stays missing,
the mock answers 404 and the recording says so (`(missing)` in the mock's
log): try again later, nothing is made up in its place.

### Choices, and the alternatives left aside

- **One script with separable steps, a thin Make target.** The steps pass
  data to each other (the version, the checksum, the migration level, which
  lists were recorded) and end in comparisons and a summary; Make would
  only have called scripts in a row. Left aside: a Make recipe chaining
  `make golden-radarr`, `make ui` and the vector commands, which writes the
  repository from the job that runs the original and says nothing about
  what changed.
- **The recordings go to the work directory, through the fixture scripts.**
  `GOLDEN_OUT` already existed; `SCHEMAS_OUT` is new, so the schemas follow.
  The scripts stay the one way an original is run. Left aside: `make
  golden-<app>` into the tree and `git diff` afterwards, which needs a
  repository write in the job without secrets.
- **How a list is recorded is read from its header.** The command each
  `requests.txt` gives (`GOLDEN_SETUP`, `GOLDEN_SIGNALR`) is parsed, and a
  variable the bump does not know stops it. Left aside: a table in the
  script, a second place to keep in step with the headers.
- **The migration level is what the new original reaches** on the fixture
  (`VersionInfo`), not the highest migration file the triage saw: a
  migration can be renumbered or removed upstream. The triage's list is in
  the summary beside it.
- **`MAX_KNOWN_MIGRATION` is not raised.** Raising it would silence the one
  test that says the importer has not been read against the new schema.
  The summary gives the file, the line and the migration files.
- **What changed is measured against `HEAD`, after normalising**: instants
  become `<time>` (any recording moves them), the recording's own instant
  and the covers' `lastWrite` are dropped; then a change is `status`,
  `shape` (keys added or gone) or `values`. A raw diff would be every file.
  `BASELINE=1` records the pinned release too and marks *noisy* what
  changes on its re-record as well; off by default, as it doubles the
  recording.
- **The vectors are regenerated whole into the work directory**, then
  replace the committed ones: `port-vectors.py` alone never removes the
  file of a fixture gone upstream.
- **The cursor moves at the end of the port, not with the pin.** The bump
  writes the pin and leaves the cursor: the triage on the port's branch
  still lists the commits to port, a run that stops halfway leaves the
  cursor where it was and the next one starts from what is left (issue #9,
  section 6), and `test_the_shipped_cursors_are_at_the_pins` fails until
  `STEPS=cursor` moves it, so a port is not green before it is done. The
  same PR still moves both. Left aside: moving the cursor with the pin,
  which says every commit is handled before any is, and leaves a run that
  stops halfway with nothing to continue from.
- **Failing tests are run once more.** A test that passes then is reported
  as flaky, not as work for the port (`golden_responses_match` in
  `crates/unarr-api/tests/contract.rs` is one).
- **The mock is the original's proxy through the environment.** .NET takes
  its default proxy from `HTTPS_PROXY`, so the original goes through the
  mock from its first request, with its settings as the fixture has them.
  Left aside: writing the proxy into the copy's settings before it starts,
  which changes `config/host` and every golden that shows it, and a
  request at the head of each list that sets it, which comes after the
  checks the original runs at start.
- **One answer of the Servarr services for every version.** Their
  answers are named without the query (version, OS, runtime), so a new
  original finds what the old one recorded. Left aside: one recording per
  version, which a bump never finds and would have to fetch, the very
  dependency this removes. The cost: the list `GET /api/v3/update` shows
  is the one recorded, not what the hosts would say to the new version,
  and the contract tests compare only the installed release's shape
  (`Mode::InstalledUpdate`).

## The workflow: up to the draft PR

`.github/workflows/upstream.yml` runs every Monday morning and on request
(*Run workflow*: the personality, and a target `X.Y.Z.BUILD` or nothing
for the rule of issue #9). Up to the draft PR it is the jobs below; on
the PR it opened, `loop-<app>` then calls `upstream-loop.yml`, which runs
the port's agent and the checks that need the original running
([The loop](#the-loop-the-agent-and-the-checks)).

Per personality, a chain of jobs; the two chains are the same steps and
an error in one does not stop the other:

| Job | Permissions | What it does |
|-----|-------------|--------------|
| `plan-<app>` | `contents: read`, `pull-requests: read` | `make upstream-plan-<app>`: the triage (the release flags from the API with the job's token), then what to do (below); when it is a bump, `make bump-<app> STEPS=fetch TARGET=<target>` downloads and checks the tarball, without running it. Hands on the plan, the fetched work directory and the tree (`git archive`) |
| `announce-<app>` | `issues: write` | a new major on the schedule: `make upstream-announce-<app>` opens the issue "<App> <version> is out" with the triage's counts, unless it is open |
| `record-<app>` | none | `make bump-<app> STEPS=record`, through `scripts/run-unprivileged.sh` (below). No checkout (the tree comes from `plan-<app>`), no token, no secret, no cache. Hands on the work directory without the original |
| `test-<app>` | none | the tree of `plan-<app>` as a local commit, `make upstream-assemble-<app>`, the other personality's web UI and `make bump-<app> STEPS=apply` as the runner's user (nothing of the new tarball runs); then, through `scripts/run-unprivileged.sh`, `cargo fetch` as a user of its own with the network and `make bump-<app> STEPS=test` as that user without it: the contract tests run the ffprobe the new original ships. Hands on `tests.json` and the logs, untrusted |
| `publish-<app>` | `contents: write`, `pull-requests: read` | `make upstream-assemble-<app>` (with the tests' results, checked), `make upstream-apply-<app>` (the extracted original removed at once: nothing of it runs here) and `make bump-<app> STEPS=summary`, then `make upstream-pr-<app>`: the commit `Pin <App> <target>`, the branch `<app>/upstream-<target>`, and, with `UPSTREAM_TOKEN`, the draft PR into the default branch with the short body of `PORTING.md` (section 1, point 4), the label of the jump (`patch`, `minor`, `major`, created when missing) and the bump's summary as its first comment. Hands on the work directory as the artifact `bump-<app>` |
| `loop-<app>` | `contents: write`, `pull-requests: write`, `issues: write`, the most the called jobs may ask for | only when `publish-<app>` opened the PR (`opened`): `upstream-loop.yml` on it, with `ANTHROPIC_API_KEY` passed by name. A major gets here only from a start by hand (on the schedule it is announced), and that start is the one issue #9 asks for: the agent goes on by itself |

What `plan` decides (`scripts/upstream-workflow.py`, tested in
`scripts/tests/test_upstream_workflow.py`), in this order:

1. **Off for the schedule.** The weekly run follows the personalities of
   `UPSTREAM_SCHEDULED` in the `Makefile`: Radarr alone, until the trial
   on Radarr is merged (issue #9, "then enable Sonarr"); adding `sonarr`
   there enables it. A manual start runs either.
2. **No new release**: nothing, green.
3. **A port already open**: an open PR of this repository from a
   `<app>/upstream-*` branch, whatever its target: nothing, green, the
   PR named ("port already open: #28"). Issue #9 never opens a second PR
   for a personality; the newer release comes with the port or after it.
4. **The branch `<app>/upstream-<target>` exists** with no open PR:
   nothing pushed over it, green, named.
5. **A new major on the schedule**: the issue, and nothing else; the port
   of a major starts by hand. A manual start bumps a major as any other.
6. Otherwise **the bump**, and the draft PR.

`publish-<app>` asks 3 and 4 again just before it pushes, and pushes
without force, so two runs never open two PRs for one personality; each
`publish-<app>` and `announce-<app>` is one at a time per personality
(`concurrency`).

**The original runs alone.** `record-<app>` runs the binary the planning
job fetched, and `test-<app>` the ffprobe it ships (the contract tests
use it); both trust nothing that job does not hand them: `record` checks
the tarball against `bump.json` before it starts, `apply` downloads it
again and checks it. On a GitHub-hosted runner the job's user has sudo and the docker
socket: what runs as that user is root, and can read from the runner's
memory the token that saves caches and artifacts, which `permissions: {}`
does not take away; a scheduled run writes the caches of the default
branch, which `ci.yml` builds the published image from. So
`sudo scripts/run-unprivileged.sh <user> <command>`, after every step
that needs root, runs the recording and the tests as a user of its own (created when
missing, refused when it is in `sudo`, `docker`, `adm` or another group
that leads to root), with a clean environment and `no_new_privs` (no
sudo, no setuid binary: it cannot become root again), in a network
namespace where only the loopback is up (the mock answers there; neither
needs the network); every process of that user is killed when it ends.
`--network` keeps the runner's network, for a step that must reach a
real service (`cargo fetch`, before anything of the tarball runs); the
rest is the same. The job that writes runs nothing of the new tarball and
reads what it is handed as untrusted: `assemble` puts `bump.json`,
`triage.json` and `triage.md` of the planning job over whatever the
recording left and checks every value of `tests.json` the summary prints,
`apply` checks before it copies and downloads the tarball again (what it
extracts is removed at once; the fixture scripts extract the whole
tarball, and only the web UI is needed), `summary` checks every value it
prints from the recording, and `pr` commits only the files the bump
writes (the pin, the goldens, the schemas, the vectors), stopping on any
other.

**Authorship.** The commit is authored and committed as `CLAUDE.md`
says, with no trailer; the title and the body name no tool.

**What the maintainer sets**: the table of "What the maintainer sets",
below, for the three workflows.

What `loop-<app>` hooks on: the outputs of `publish-<app>` (`pr`,
`branch`, `app`, `target`, `jump`, `url`, and `opened`, `true` only when
this run opened the PR). The artifacts of the run (`plan-<app>`,
`bump-<app>`) stay for a person.

Locally, with a releases file in place of the API and GitHub asked
anonymously (the repository is public):

```sh
GITHUB_REPOSITORY=bateman/unarr EVENT=schedule make upstream-plan-radarr RELEASES=<releases.json>
```

### Choices, and the alternatives left aside

- **Two chains of jobs written once, by YAML anchors.** A matrix shares
  one set of job outputs between its legs and joins them at every
  `needs`; a reusable workflow is a second file. Left aside: both.
- **The tarball is fetched by the planning job**, with the job's read
  token for the API, and handed on: the recording job has no token at all,
  and the API's anonymous limit is shared by every job of the runner's
  address.
- **The recording runs with no network, not only with no secret.** The
  job has nothing to steal, but a runner's caches are shared with the
  other workflows of the branch: a binary that could reach the network
  and the runner's credentials could poison them. Left aside: trusting
  the mock as the original's proxy alone, which the original can ignore.
- **The tests in a job without a token.** The contract tests run the
  ffprobe of the new tarball. Left aside: running them in the job that
  writes, as the runner's user (root, the write token, the default
  branch's caches), or without that ffprobe, when the goldens were
  recorded with it and the runner's own, if any, can read media apart.
- **The PR opened only with `UPSTREAM_TOKEN`.** Left aside: the job's
  own token, which needs the setting that also lets a job's token
  approve a PR, and opens a PR on which CI does not start.
- **Any open port of the personality stops the run**, not only one for
  the same target: issue #9 ("A PR for that personality is still open:
  add to it; do not open a second").

## The checks with the original, and the port's agent

Two workflows act on a port's PR (`<app>/upstream-<version>`), each
started on request from the Actions tab with the PR's number
(`workflow_dispatch`), or called by `upstream-loop.yml` with the same inputs
(`workflow_call`; below). The logic is in `scripts/upstream-checks.py` and
`scripts/upstream-port.py`, tested by `make test-scripts`; the YAML calls
`make upstream-checks-*` and `make upstream-port-plan`.

### `upstream-checks.yml`: what needs the original running

`PORTING.md`, section 10. Inputs: `recordings` (`recyclarr`, or `none`;
`transmission` and `sabnzbd` are named but left to a session),
`http_audit`, `compare` (`none`, `real`, `scaled`, `both`), `replays`,
`old_pin_request` and `old_pin_wait` (a golden that differs by timing).

| Job | Permissions | What it does |
|-----|-------------|--------------|
| `plan` | `contents: read`, `pull-requests: read` | refuses a PR that is not a port's of this repository into `main`, and inputs out of shape (`make upstream-checks-plan`) |
| `run` | none | the PR's head, `make ui`, `make build`; then, through `scripts/run-unprivileged.sh` (as `upstream.yml` runs its recording), as the user `unarr-checks`: `make upstream-checks-prepare` with the network (Recyclarr 8.8.1, SHA-256 checked; TRaSH-Guides at its commit; `main` and the tarball of its pin, checked by its fixture script: nothing of these runs here), then `make upstream-checks-run` with no network but the loopback: the checks in the order of section 10, the originals started with `SHELL_VIA_MOCK=1`, so that the mock answers for the Servarr services as it does when recording. No secret, no credential left in git, caches restored and never saved. Hands `upstream-checks-results-<pr>-<commit>` (recordings, reports, `report.json`; links and special files dropped) and the same name with `-logs` on as artifacts: one name per PR and commit, since a run of `upstream.yml` may run the checks of both personalities, and one of `upstream-loop.yml` the checks twice on one PR |
| `apply` | `contents: write`, `pull-requests: write` | refuses unless `main` is protected from its token (`make upstream-main-protected`, below); `main`'s script reads the results as untrusted (`make upstream-checks-apply`): only the files it expects, at their paths, regular, of a sane size, JSON where JSON, every value it prints checked (`checked_file` of `pin-bump.py`); commits a recording whose test compares answers (one commit each) when it says something the committed one does not, and the comparison (one commit), on the commit the checks ran at; pushes without force; comments the report on the PR |

Why a user of its own: on a hosted runner the runner's user has `sudo`
without a password and the docker socket, so it is root in all but name.
From root, the runtime token the runner holds for caches and artifacts
can be read from its process even in a job with no permission, and a cache
entry written with it in `main`'s scope would reach CI, which restores
Rust's cache and builds the published image from GitHub's cache. What
counts is who can write a cache, not which step saves one; so the
originals run as a user that is not root and cannot become it
(`run-unprivileged.sh`: no `sudo`, `docker` or `adm` group,
`no_new_privs`), and so cannot read another user's processes, the
runner's among them; the runner's home is closed to it. Not verified on a
hosted runner, only on a container of the same Ubuntu.

Two recordings of the Recyclarr exchange with the same Recyclarr, guide,
original and fixture are not the same bytes: Recyclarr sends a profile's
`formatItems` in an order of its own, which the original stores and
answers back, and lists names in its output in that order; `system/status`
gives the paths, the OS and the start time of the machine. So `apply`
commits a new exchange only when `differences` of
`scripts/recyclarr-replay.py` (requests paired by method and path,
`system/status`'s own values left out) finds something once those orders
are sorted (`exchange_differences` in `scripts/upstream-checks.py`); the
first run on #28 committed 3,500 lines that said nothing.

The audit runs the original with the branch's own fixture script. On a
port's branch opened before `main` had `SHELL_VIA_MOCK`, the original
reaches no Servarr service, and a route they answer (`GET /api/v3/update`)
fails there and not in Unarr: the report says so beside the audit's
outcome, and the branch takes `main` before the audit is made again. The
line make writes on the audit's exit status is left out of the report.

What it leaves: the download clients' captures, whose test follows their
sequence numbers and info hashes, and any recording a job could commit
only by changing its test; the metadata answers (`RECORD_METADATA=1`); the
prose of `tests/fixtures/recyclarr/README.md`. A session does them
(`PORTING.md`, section 10).

### `upstream-port.yml`: the port's agent

The agent follows `PORTING.md` on a port's **draft** PR. Its prompt is
`upstream/port-prompt.md`, filled from checked values only (the PR's
number, its branch, the limits); the PR's own text never enters it.

| Job | Permissions | What it does |
|-----|-------------|--------------|
| `plan` | `contents: read`, `pull-requests: read` | refuses what is not a port's draft PR, and a run without `ANTHROPIC_API_KEY`; the run's limits by the jump (the label `major` gets the higher ones: `LIMITS` in `scripts/upstream-port.py`), its spend cut to its share when `upstream-loop.yml` calls it (the input `round`); the prompt (`make upstream-port-plan`) |
| `port` | `contents: write`, `pull-requests: write`, `issues: write` | the branch, `make ui` (the web UI alone), then the agent, with a deadline in its prompt and the job's stop 30 minutes later, `--max-turns`, `--max-budget-usd`; then `main`'s script checks what the run left |

- **No original.** `UNARR_NO_ORIGINAL=1` in the job: the fixture scripts
  extract the web UI alone, never the binary, and refuse `golden` and
  `shell`; `compare-originals.py` (and `route-percentiles.py`) refuse to
  start; the agent's tool rules deny the Make targets that run one. The
  guard that holds is that no original is on disk.
- **What the tool rules are worth.** `--allowedTools` and
  `--disallowedTools` match a command's prefix: they keep an agent from a
  mistake, not from a way round. `python3`, `cargo` and `git` are
  allowed, and any of them runs arbitrary code and reaches the network.
  What an injected instruction could do is bounded by the job, not by
  the rules: the job's own token (no other enters the job), which a
  ruleset keeps off `main`; the API key, which is not in the environment
  of the agent's commands (`CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1`, with
  bubblewrap) but is in that of the agent's own process; and the key's
  spend limit.
- **Authorship.** `GIT_AUTHOR_*` and `GIT_COMMITTER_*` are the
  maintainer's in the job's environment, which wins over the
  `claude[bot]` identity the action writes in git's configuration; the
  setting `attribution` is empty, so the agent adds no co-author and no
  line naming the tool to commits and PRs; a `commit-msg` hook from
  `main` refuses a trailer or a tool's name; after the run, `verify`
  checks every commit added (author, committer, message), that the
  branch only moved forward, and the PR's title and body.
- **What it may do on GitHub.** `gh` for the PR (view, edit, comment,
  ready), for issues (view, list, create) and for the clients' releases;
  review comments through `scripts/upstream-port.py review`, a review of
  kind "comment". `gh api`, `gh pr merge`, `gh pr review`, workflows,
  secrets and force pushes are denied by the tool rules, with the limit
  said above; the job's token cannot touch workflows or secrets in any
  case.
- **main.** A token with `contents: write` can push to `main` and merge a
  PR through the API. Before the agent starts, and in `apply` of the
  checks before any push, `make upstream-main-protected` asks GitHub for
  the rules of `main` and refuses to go on unless an active ruleset there
  has "Restrict updates" (only who is in its bypass list updates the
  branch) and the job's token cannot bypass it (`current_user_can_bypass`
  is `never`). No approval is asked of anyone: the maintainer, in the
  bypass list, merges as before.
- **CI.** Pushes made with the job's own token start no workflow: CI runs
  on the port's commits at the next push of a person (or "Update branch"
  on the PR). No stronger token is given to the agent for this.
- **A run cut short.** The agent is told the deadline and does what
  fixed rule 5 asks before it; a run stopped all the same loses only
  what was not pushed, and the next run starts from the PR's table and
  the log (`PORTING.md`, section 1, "Decisions without their log row").

## The loop: the agent and the checks

`.github/workflows/upstream-loop.yml` joins the two workflows above on one
port's draft PR. `upstream.yml` calls it on the PR it has just opened
(`loop-<app>`); started on request with a PR's number, it goes on where a
run stopped. The agent cannot start a workflow (its token has no
`actions: write`, and gets none); what it can do is write, at the end of
the PR body, the block `upstream-next` (`PORTING.md`, section 7, "What
comes next"), a fixed set of keys: `next` (`port`, `checks`, `done`), the
commit it pushed last, and for `checks` the inputs of `upstream-checks.yml`.
After each run of the agent, a job with a read token checks out `main` and
runs `make upstream-loop-next` (`scripts/upstream-port.py next`), which
reads that block alone, checks every key and value (the inputs as
`upstream-checks.yml` checks them), and starts what it asks for:

| Job | Permissions | What it does |
|-----|-------------|--------------|
| `ready` | `contents: read`, `pull-requests: read` | `make upstream-loop-ready`: a port's draft PR, the secret `ANTHROPIC_API_KEY` set (the job sees only whether it is), `main` protected from the workflows' token (`make upstream-main-protected`'s check). When one is missing, it says which in the run's summary, and the run ends green with nothing else started |
| `port-1`, `port-2`, `port-3` | `contents: write`, `pull-requests: write`, `issues: write` (what `upstream-port.yml` asks for) | `upstream-port.yml` with the PR and the round; the key passed by name |
| `next-1`, `next-2`, `next-3` | `contents: read`, `pull-requests: read` | after run *n* of the agent: `port` (run *n*+1), `checks` (the checks, then run *n*+1 on their report), or `stop`. After run 3, it only says what the block asks for |
| `checks-1`, `checks-2` | `contents: write`, `pull-requests: write` (what `upstream-checks.yml` asks for) | `upstream-checks.yml` with the block's inputs; no secret |

`next` stops the loop, green, when the agent's run did not succeed (its
limit hit hard, a failure, a cancel: the run is red there already), when
the PR is no longer a draft, on `next: done`, after the third run, and
when the checks are asked again at a commit they already ran at in this
run. It stops red when the body has no block, more than one, one out of
shape, or one that names another commit than the branch's head (written
before the last push, or by an earlier run): the loop never guesses what
the agent meant. Either way the PR stays a draft with its body, table and
log, and `upstream-loop` started by hand with its number goes on from
there; so do `upstream-port.yml` and `upstream-checks.yml` started alone.
A loop started by hand begins with a run of the agent: when the block
already asks for the checks, start `upstream-checks.yml` with its inputs
first, then the loop, so that no run of the agent only writes the block
again.

**The limits.** The rounds are written out in the YAML: at most three
runs of the agent and two runs of the checks per run of the workflow
(`ROUNDS` in `scripts/upstream-port.py`; a test holds the YAML to it).
Nothing loops: no job starts a run of a workflow, and no push does (the
job's token starts none). Each run of the agent keeps the minutes and
turns of its jump (`LIMITS`) and spends at most its share of the loop's
cap (`ROUND_BUDGETS`): the first, the port itself, the largest; the last,
which settles what the checks found and moves the cursor, the smallest.

| Jump | Runs of the agent, USD each | At most, per run of the loop and PR | Time, at most |
|------|-----------------------------|--------------------------------------|---------------|
| patch, minor | 20, 12, 8 | 40 USD | 3 jobs of 3 h 30 min (2 h of the agent each, 250 turns), and 2 runs of the checks (5 h 20 min each, the comparison included) |
| major | 50, 30, 20 | 100 USD | 3 jobs of 5 h 30 min (4 h of the agent each, 500 turns), and the same checks |

`upstream-port.yml` started alone is the first run: 20 USD for a patch or
minor, 50 for a major. A run of `upstream.yml` may open the PRs of both
personalities, and so spend up to twice the cap; a month with a patch or
minor of each stays within 80 USD, and a major, which starts only by hand,
within 100. Every job has its timeout, within GitHub's six hours; a whole
run stays far under its limit of 35 days; each job has its own token,
which lasts the job.

**These limits are per run, not per month.** Nothing here counts what
earlier runs spent: the true monthly cap is the one the maintainer sets on
the key itself, in the Claude Console (below), and a run that reaches it
stops there, its PR left as a stopped loop leaves it.

**Concurrency.** One loop per PR (`upstream-loop-pr-<n>`); the called
workflows keep their own groups (`upstream-port-pr-<n>`,
`upstream-checks-pr-<n>`), never the caller's, and the jobs that call
them hold none: a loop never waits on itself. A run of either started by
hand on the same PR waits its turn; GitHub keeps one waiting per group
and cancels an older one, which stops the loop where it was.

**Security.** As in the workflows it calls: `permissions: {}` at the top
and each job its own; the jobs that decide hold a read token, check out
`main` and read the PR's body as data; the key goes by name, never
`secrets: inherit`, to `upstream-port.yml` alone, which hands it to the
agent's step; the checks get no secret; no `${{ }}` in a `run:`.

**Majors.** On the schedule a major is announced, not bumped, so no PR and
no agent. A bump of a major started by hand opens its PR, and the agent
starts on it as on any other: issue #9 makes that start the one by hand.

### Choices, and the alternatives left aside

- **The signal is a block of fixed keys in the PR body**, read by `main`'s
  script. Left aside: a label (it cannot carry the checks' inputs, nor
  the commit they are for); free text read by another model; the agent
  starting the workflow itself (`actions: write`, which also edits
  workflows' runs).
- **The rounds written out as jobs.** Left aside: a workflow that calls
  itself or is started again by a push or a dispatch, which nothing
  bounds but a counter kept somewhere the agent can write.
- **Each round a share of a total**, decided before the run. Left aside:
  reading what each run spent, which the agent's job reports and the
  agent could change.
- **A fourth file** for the loop, called by `upstream.yml` and started by
  hand alike. Left aside: the rounds written twice in `upstream.yml`, once
  per personality, with no way to go on by hand.

## What the maintainer sets

For the four workflows (`upstream.yml`, `upstream-loop.yml`,
`upstream-port.yml`, `upstream-checks.yml`). What runs by itself, for a new
release found on Monday (or a start by hand), with each combination:

| Set | `upstream.yml` | Then |
|-----|----------------|------|
| nothing | the bump, the branch pushed, green; the run's summary gives the link to open the PR by hand | nothing starts. Once the PR is open, `upstream-checks.yml` on request runs the checks and leaves their results in its artifact, committing nothing; `upstream-port.yml` and `upstream-loop.yml` refuse or stop, saying what is missing |
| `UPSTREAM_TOKEN` only | the draft PR opened, labelled, CI on it | `upstream-loop.yml` starts on it and ends green at once, saying that `ANTHROPIC_API_KEY` is missing (or, with the key and no ruleset, that `main` is not protected): no agent, no checks |
| all three | the draft PR opened | the loop: the agent, the checks when the port asks for them, the agent again on their report, within the limits above. It stops when the port is done or waits on someone, and the PR stays a draft until a person marks it ready or the port does (section 9) |

With the key and the ruleset but no `UPSTREAM_TOKEN`, the PR is opened by
hand and the loop is then started by hand with its number.

| What | Where | For | Without it |
|------|-------|-----|------------|
| A ruleset on `main` with "Restrict updates" (steps below) | Settings, Rules, Rulesets | `upstream-port.yml` and `apply` of `upstream-checks.yml`: their token may write contents, and with it push to `main` or merge a PR through the API; the rule keeps it off `main`, and `make upstream-main-protected` checks it | the loop ends green saying so; the agent does not start; `apply` commits and pushes nothing (the report stays in the artifact) |
| The secret `ANTHROPIC_API_KEY`: a key for this alone, in a workspace with a monthly spend limit (Claude Console), not the account's OAuth token | Settings, Secrets and variables, Actions | `upstream-port.yml`: the agent. It reads untrusted upstream text and its own process holds the key, so an injected instruction could get the key out: a key of its own with a limit bounds the cost, and is revoked alone | the loop ends green saying so; the agent does not start |
| The secret `UPSTREAM_TOKEN` (optional): a fine-grained token for this repository only, *Contents* and *Pull requests* read and write | Settings, Secrets and variables, Actions | `upstream.yml`, in `publish-<app>` alone (no agent, nothing untrusted runs there): it opens the draft PR, and CI starts on it; the loop starts only on a PR so opened | the branch is pushed with the job's token and the PR is opened by hand, from the link in the run's summary; the loop is then started by hand |

Nothing else needs setting:

- "Allow GitHub Actions to create and approve pull requests" (Settings,
  Actions, General): leave it off. No workflow needs it, and on it would
  let a job's token approve a PR; with "Restrict updates" on `main` an
  approval opens no way to `main`.
- The workflow permissions' default: each job asks for its own.
- The labels `patch` and `minor`: created at their first use; `major`
  exists.
- Rules on the ports' branches: none needed. `verify`, after the agent,
  fails the run on a branch rewritten.

**The ruleset on `main`**, from the repository's page:

1. *Settings*, then in the sidebar *Rules*, *Rulesets*; *New ruleset*,
   *New branch ruleset*.
2. *Ruleset name*: `main`. *Enforcement status*: *Active*.
3. *Bypass list*: *Add bypass*, choose *Repository admin*, *Add
   selected* (mode *Always allow*). Nothing else: not *Maintain*, not
   *Write*, no app.
4. *Target branches*: *Add target*, *Include default branch*.
5. *Branch rules*: tick *Restrict updates*; leave *Restrict deletions*
   and *Block force pushes* ticked. Nothing else.
6. *Create*.

The test after it: from a clone,
`GITHUB_REPOSITORY=bateman/unarr GH_TOKEN=$(gh auth token) make upstream-main-protected`
must refuse, naming the ruleset and the bypass `'always'`: that is the
maintainer's own token, in the bypass list. The first run of either
workflow must print, in its first steps, "main restricts updates to its
bypass list, and this token cannot bypass it".

Not verified: GitHub's page says that "Restrict updates" lets only those
in the bypass list *push* to the branch; that it also refuses the merge
of a PR through the API by the job's token is how a merge works (it
updates the branch), not something the page says. Should it not, the
alternative that asks no approval of anyone is the classic branch
protection's "Lock branch" on `main` (the branch read-only), with
administrators left free to bypass it.
