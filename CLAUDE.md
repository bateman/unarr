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

## Merging a branch

To merge a branch means to open a pull request for it into `main` and
merge that pull request on GitHub, with a merge commit. Never merge it
locally and push `main`: GitHub deletes a pull request's head branch when
the pull request is merged, and only then. Do not ask whether to delete
merged branches, and do not delete them yourself.

## Developing a plan in steps

A plan in steps (a phase work order `docs/phase-*.md` with its milestones,
or an issue with a checklist) is developed as follows, unless the user says
otherwise for that plan.

1. **The plan lives on GitHub.** One checklist item per step in the issue,
   or in a `docs/phase-*.md` the issue links. A plan that exists only in a
   chat is copied there first: a new session cannot read a claude.ai
   conversation.
2. **The main conversation runs on Fable and only coordinates.** It hands
   each step off, reviews it, updates the issue and reports to the user. It
   writes no step's code.
3. **Each step is developed in a new session on Opus** (`create_session`,
   this repository). Its handoff prompt is all it knows: the issue and the
   step, the scope and exit criteria, the main session's id, and the branch
   name as `outcome_branch`, named as above (without it the session gets a
   branch this file forbids). A step split over several flavors gets one
   session per branch. The session opens a PR into `main` for its branch
   (this counts as the user asking for it), waits for CI to be green, never
   merges, and ends by sending the main session a report: branch, PR, CI,
   what it did not do.
4. **Effort**: the model's default. Leave `effort` unset unless the user
   names a level.
5. **Review.** A step is done when its branches have `make ui && make check`
   green, `docs/originals-comparison.md` updated when the step counts (see
   below), the PR open and CI green. The main conversation then runs
   `/ponytail-review` on each branch, once: no review loop.
6. **Fixes.** The findings go back to the step's session, which handles
   them under `/ponytail ultra`: each finding is questioned, the justified
   ones are fixed on the same branch. Rejected findings, with the reason,
   and findings outside the step or already on `main` go in the issue, not
   in the branch. Then `make ui && make check` and CI green again.
7. **The issue.** The main conversation ticks the step in the checklist and
   comments: the PRs, what the step did, the findings fixed, those rejected
   or deferred and why.
8. **The user decides.** The main conversation tells the user what is left
   to do, or that the PRs can be merged. The user merges (see above); the
   next step starts from `main` only after the previous one is merged.

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
- The same holds for a comment on an issue or a pull request, a review
  and a reply to one included: no footer and no line that names a tool, an
  agent, a model or a session. When a tool adds one by itself, to a comment
  or to the body of a pull request, remove it.

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
