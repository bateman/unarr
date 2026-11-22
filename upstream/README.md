# Following the originals upstream

How Unarr follows the Sonarr and Radarr releases (issue #9). Per
personality:

| File | What | Written by |
|------|------|------------|
| `<app>.pin` | the release Unarr emulates: version, build time, migration level, SHA-256 of the tarball; the one place a pinned value is written (`scripts/tests/test_pins.py`) | `make bump-<app>` |
| `<app>.lock` | the cursor: the last upstream commit handled, and its release | `make bump-<app> STEPS=cursor`, the last step of a port |
| `<app>-wontport.md` | upstream changes Unarr does not take, so the triage does not assess them again | by hand |

Two commands, run in this order:

- `make upstream-<app>` (`scripts/upstream-triage.py`): which stable release
  to go to, and a table of the upstream commits since the cursor, classified
  by the paths they touch. It changes nothing.
- `make bump-<app>` (`scripts/pin-bump.py`): brings the tree to that release
  and says what changed. Its summary, with the triage's table, is where a
  port starts.

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
| `apply` | the pin, not the cursor; the recordings, compared with `HEAD` first; the old binary removed from the cache and `make ui-<app>`, which downloads the tarball again and checks it against the new pin; the vectors of the whole new tag (`scripts/port-vectors.py`), files gone upstream removed; the importer's `MAX_KNOWN_MIGRATION` read | the repository | GitHub | no |
| `test` | `cargo test --workspace --no-fail-fast` and `make test-scripts`; each failing test run once more (20 at most), to tell a flaky one | the work directory | none | no |
| `summary` | the release, which recordings changed and how (by list and by area), the schemas and strings, the vectors, the importer, the failing tests and what each printed first, what is left to check by hand, the time each step took | the work directory | none | no |
| `cursor` | not a default step: once every upstream commit up to the pinned release is handled, the cursor moves to that release and the commit of its tag, read from the triage's clone | the repository | GitHub | no |

`record` is the only step that runs a binary nobody vetted, and it writes
nothing in the repository: in the workflow it runs in a job without
secrets, which hands the work directory on as an artifact. `apply` reads
that directory as untrusted: it copies only the files the request lists
name, refuses links, files over 64 MiB and JSON that does not parse, checks
the values it pins, and never runs the original. The tarball the UI comes
from is downloaded again by `apply` and checked against the checksum
`fetch` computed, so a work directory changed by the original cannot change
it either.

What the bump leaves to the port, and lists in the summary:

- the importer's `MAX_KNOWN_MIGRATION`, when the new original migrates
  further: `the_pinned_release_is_within_the_known_migrations` fails until
  the mapping has been read against the new migrations, then it is raised
  and a test imports a database at the new level (the fixture the new
  original migrated is in the work directory);
- the Recyclarr exchanges (`tests/fixtures/recyclarr/`), recorded between
  the real Recyclarr and the real original;
- the metadata answers (`tests/fixtures/metadata/`): the summary lists the
  requests the mock had no answer for (`RECORD_METADATA=1` records them);
- the Bazarr and Prowlarr replays, written from those projects' source;
- the prose that names the old release, and `docs/originals-comparison.md`
  as `CLAUDE.md` says.

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
