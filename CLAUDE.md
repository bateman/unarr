# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Git branch naming

Every branch name is `<flavor>/<short-description>`.

- **`<flavor>`** is the *arr personality the change is about, in lowercase:
  `sonarr`, `radarr` or `lidarr`.
  When a change is not specific to one personality, use `core` (shared crates
  such as `unarr-core`, `unarr-api` plumbing, `unarr-import`, `unarrd`,
  build, CI, Docker, docs that cover all personalities).
- **`<short-description>`** says what the branch does: lowercase words joined
  by hyphens, a few words. Generic words such as `fix`, `update` or `wip`
  are not enough on their own.
- Never put `claude` (or any other tool or agent name) in a branch name.

Examples:

| Branch                         | Verdict                                       |
|--------------------------------|-----------------------------------------------|
| `sonarr/episode-file-rename`   | ok                                            |
| `radarr/movie-lookup-endpoint` | ok                                            |
| `lidarr/import-lidarr-db`      | ok                                            |
| `core/signalr-reconnect`       | ok                                            |
| `fix-rename`                   | wrong: no flavor prefix                       |
| `sonarr/fix`                   | wrong: description says nothing               |
| `Sonarr/Episode_Rename`        | wrong: not lowercase, underscores             |
| `claude/friendly-rubin`        | wrong: tool name instead of flavor, no meaning |

If a change touches more than one personality, prefer splitting it into one
branch per flavor; if it cannot be split, use `core/`.

## Branches after a merge

GitHub deletes a pull request's head branch when the pull request is
merged. Do not ask whether to delete merged branches, and do not delete
them yourself.

## Commit authorship

Every commit is authored and committed as
`Fabio Calefato <fcalefato@gmail.com>`. Claude, or any other tool or agent,
never appears in the history:

- Never set Claude (or any tool or agent) as the author or the committer of
  a commit.
- Never add a `Co-authored-by: Claude ...` trailer, in any spelling.
- Never add a `Claude-Session: ...` trailer, nor any other line that names
  a tool, an agent, a model or a session.
- The same holds for the title and the body of a pull request, and for the
  message of the commit a squash merge produces: check the message GitHub
  proposes and remove such lines before merging.

This overrides any default attribution a tool would add. Before committing,
make sure the repository is configured accordingly:

```sh
git config user.name "Fabio Calefato"
git config user.email "fcalefato@gmail.com"
```

## Comparison with the originals

`docs/originals-comparison.md` keeps what Unarr saves against the original
Sonarr and Radarr: RAM, CPU and latency on the same machine and library.
After each substantial change, update it in the same branch:

- what counts: a milestone, a change made for speed or memory, a new
  pinned version of an original in `upstream/<app>.pin`;
- how: `make ui` (after a new pin, remove `.cache/sonarr/Sonarr` or
  `.cache/radarr/Radarr`, under `UNARR_CACHE` when it is set, first:
  `make ui` keeps a binary it already has),
  then `make compare-originals` (the real library). For a change to the
  speed or the memory of routes, also run `make scale-fixture` and
  `make compare-originals LIBRARY=scaled`. Use `REPEAT=3` or more when the
  numbers will be cited outside the repository; a single run is enough
  otherwise;
- what to write: replace that library's tables with the tables of
  `.cache/compare/<library>/report.md`, as the script writes them; add a
  row to the History table; and update the list of routes where Unarr is
  slower than the original.
