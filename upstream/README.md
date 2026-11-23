# Following the originals upstream

How Unarr follows the Sonarr and Radarr releases (issue #9). Per
personality:

| File | What | Written by |
|------|------|------------|
| `<app>.pin` | the release Unarr emulates: version, build time, migration level, SHA-256 of the tarball; the one place a pinned value is written (`scripts/tests/test_pins.py`) | `make bump-<app>` |
| `<app>.lock` | the cursor: the last upstream commit handled, and its release | `make bump-<app> STEPS=cursor`, the last step of a port |
| `<app>-wontport.md` | upstream changes Unarr does not take, so the triage does not assess them again | by hand |
| `<app>-log.csv` | one row per upstream commit a port handled: its class, the decision, the Unarr commits, what a human changed in review (`scripts/tests/test_port_log.py`) | the port, at the end of each run: the decisions made final in it |
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
  the PR's branch (issue #9); until that job exists, the port lists it as
  left to check by hand and its PR is not merged without it
  (`PORTING.md`, section 8).

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
