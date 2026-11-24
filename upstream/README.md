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

The workflow `upstream` runs both every Monday and on request, and opens
the draft PR the port starts from: see
[The workflow: up to the draft PR](#the-workflow-up-to-the-draft-pr).

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
for the rule of issue #9). It ends with the draft PR open: the port's
agent, and the checks that need the original running, are workflows of
their own, joined to this one in a later step.

Per personality, a chain of jobs; the two chains are the same steps and
an error in one does not stop the other:

| Job | Permissions | What it does |
|-----|-------------|--------------|
| `plan-<app>` | `contents: read`, `pull-requests: read` | `make upstream-plan-<app>`: the triage (the release flags from the API with the job's token), then what to do (below); when it is a bump, `make bump-<app> STEPS=fetch TARGET=<target>` downloads and checks the tarball, without running it. Hands on the plan, the fetched work directory and the tree (`git archive`) |
| `announce-<app>` | `issues: write` | a new major on the schedule: `make upstream-announce-<app>` opens the issue "<App> <version> is out" with the triage's counts, unless it is open |
| `record-<app>` | none | `make bump-<app> STEPS=record`, through `scripts/run-unprivileged.sh` (below). No checkout (the tree comes from `plan-<app>`), no token, no secret, no cache. Hands on the work directory without the original |
| `test-<app>` | none | the tree of `plan-<app>` as a local commit, `make upstream-assemble-<app>`, the other personality's web UI and `make bump-<app> STEPS=apply` as the runner's user (nothing of the new tarball runs); then, through `scripts/run-unprivileged.sh`, `cargo fetch` as a user of its own with the network and `make bump-<app> STEPS=test` as that user without it: the contract tests run the ffprobe the new original ships. Hands on `tests.json` and the logs, untrusted |
| `publish-<app>` | `contents: write`, `pull-requests: read` | `make upstream-assemble-<app>` (with the tests' results, checked), `make upstream-apply-<app>` (the extracted original removed at once: nothing of it runs here) and `make bump-<app> STEPS=summary`, then `make upstream-pr-<app>`: the commit `Pin <App> <target>`, the branch `<app>/upstream-<target>`, and, with `UPSTREAM_TOKEN`, the draft PR into the default branch with the short body of `PORTING.md` (section 1, point 4), the label of the jump (`patch`, `minor`, `major`, created when missing) and the bump's summary as its first comment. Hands on the work directory as the artifact `bump-<app>` |

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

**What the maintainer sets in the repository:**

- The secret `UPSTREAM_TOKEN`: a fine-grained personal access token (or
  a GitHub App's) for this repository only, with *Contents* and *Pull
  requests* read and write. `publish-<app>` pushes and opens the PR with
  it, so CI starts on the PR; it reaches only that step. Without it the
  job pushes the branch with its own token and stops there: the run's
  summary gives the link to open the PR, and the title, the body and the
  first comment are in the artifact `bump-<app>` (`pr-title.txt`,
  `pr-body.md`, `pr-comment.md`); the next runs find the branch and do
  nothing until its PR is open.
- *Settings, Actions, General, Workflow permissions*: "Allow GitHub
  Actions to create and approve pull requests" stays **off**. The job's
  token never opens a PR, and with that setting on it could also approve
  one, which the port's workflow must not be able to do. Each job asks
  for its own permissions, so the token's default can stay read-only.
- The labels `patch` and `minor` are created at their first use (with
  `UPSTREAM_TOKEN`); `major` exists.

What a later step hooks on: the outputs of `publish-<app>` (`pr`,
`branch`, `app`, `target`, `jump`, `url`) and the artifacts of the run
(`plan-<app>`, `bump-<app>`).

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
(`workflow_dispatch`), or called by another workflow with the same inputs
(`workflow_call`). The logic is in `scripts/upstream-checks.py` and
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
| `run` | none | the PR's head, `make ui`, `make build`, then the checks in the order of section 10 (`make upstream-checks-run`): the originals, Recyclarr 8.8.1 (SHA-256 checked) and TRaSH-Guides at its commit run here, as a user of their own (`originals`: no sudo, not in the `docker` group, the runner's home closed to it; the step checks it cannot read the runner's processes). No secret, no credential left in git, caches restored and never saved. Hands `upstream-checks-results` (recordings, reports, `report.json`, links and special files dropped) and `upstream-checks-logs` on as artifacts |
| `apply` | `contents: write`, `pull-requests: write` | refuses unless `main` is protected from its token (`make upstream-main-protected`, below); `main`'s script reads the results as untrusted (`make upstream-checks-apply`): only the files it expects, at their paths, regular, of a sane size, JSON where JSON, every value it prints checked (`checked_file` of `pin-bump.py`); commits a recording whose test compares answers (one commit each) and the comparison (one commit), on the commit the checks ran at; pushes without force; comments the report on the PR |

Why a user of their own: on a hosted runner the runner's user has `sudo`
without a password and the docker socket, so it is root in all but name.
From root, the runtime token the runner holds for caches and artifacts
can be read from its process even in a job with no permission, and a cache
entry written with it in `main`'s scope would reach CI, which restores
Rust's cache and builds the published image from GitHub's cache. What
counts is who can write a cache, not which step saves one; so the
originals run where they cannot reach that token. Not verified here: that
a hosted runner keeps no other way from that user to the runner (the step
checks the `environ` of the runner's processes, `sudo` and the docker
socket, and prints `kernel.yama.ptrace_scope`).

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
| `plan` | `contents: read`, `pull-requests: read` | refuses what is not a port's draft PR; the run's limits by the jump (the label `major` gets the higher ones: `LIMITS` in `scripts/upstream-port.py`); the prompt (`make upstream-port-plan`) |
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
  the rules of `main` and refuses to go on unless a ruleset there
  requires a pull request with at least one approval and the job's token
  cannot bypass it (`current_user_can_bypass` is `never`). The job's
  token cannot give that approval while "Allow GitHub Actions to create
  and approve pull requests" is off, a setting a job cannot read: keep it
  off.
- **CI.** Pushes made with the job's own token start no workflow: CI runs
  on the port's commits at the next push of a person (or "Update branch"
  on the PR). No stronger token is given to the agent for this.
- **A run cut short.** The agent is told the deadline and does what
  fixed rule 5 asks before it; a run stopped all the same loses only
  what was not pushed, and the next run starts from the PR's table and
  the log (`PORTING.md`, section 1, "Decisions without their log row").

### What the maintainer sets

- **The secret** (Settings > Secrets and variables > Actions):
  `ANTHROPIC_API_KEY`, a key made for this workflow alone, in a workspace
  with a monthly spend limit (Claude Console). Not the account's OAuth
  token: the agent reads untrusted upstream text, and its own process
  holds the key, so an injected instruction could get the key out; a
  dedicated key with a limit bounds what that costs, and can be revoked
  alone.
- **Settings > Actions > General:** "Allow GitHub Actions to create and
  approve pull requests" off; workflow permissions "Read repository
  contents" (each job asks for what it needs).
- **A ruleset on `main`** (Settings > Rules > Rulesets), active:
  "Require a pull request before merging" with at least one required
  approval, and no bypass for GitHub Actions or a role the job's token
  has. A single maintainer who merges their own PRs adds the Repository
  admin role to the bypass list, in "pull requests only" mode: that
  bypass is the maintainer's, not the job token's. Without such a
  ruleset both workflows refuse to write.
- **A ruleset on `sonarr/upstream-*` and `radarr/upstream-*`** that blocks
  force pushes and deletion: `verify` sees a rewrite only after it.
