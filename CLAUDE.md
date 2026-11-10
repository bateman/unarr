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
