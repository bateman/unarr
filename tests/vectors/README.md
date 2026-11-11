# Ported test vectors

Test cases extracted from the Sonarr and Radarr repositories (GPL-3.0, as
Unarr) by
`scripts/port-vectors.py`, one JSON file per NUnit fixture:
`{"source": "<file in the original's tree>", "methods": {"<test method>":
{"params": [...], "cases": [[arg, ...], ...]}}}`. Each method's arguments are
decoded C# literals; `Quality.X` and `Language.X` references are kept as
`{"enum": "Quality", "member": "X"}`.

| Directory | Source | Replayed by |
|-----------|--------|-------------|
| `parser/` | `src/NzbDrone.Core.Test/ParserTests/` at `v4.0.20.3014` | `crates/unarr-core/tests/parser_vectors.rs` |
| `decisionengine/` | `src/NzbDrone.Core.Test/DecisionEngineTests/` at `v4.0.20.3014` (the `TestCase`-driven fixtures; the others assert on mocks and are ported as unit tests in `crates/unarr-core/src/decision/`) | `crates/unarr-core/tests/decision_vectors.rs` |
| `radarr/decisionengine/` | Radarr's `src/NzbDrone.Core.Test/DecisionEngineTests/` at `v6.4.4.10685` (the `TestCase`-driven fixtures; the others, and `ParserTests/ParsingServiceTests`, assert on mocks and are ported as unit tests in `crates/unarr-core/src/movies/decision/tests.rs`) | `crates/unarr-core/tests/movie_decision_vectors.rs` |
| `radarr/parser/` | Radarr's `src/NzbDrone.Core.Test/ParserTests/` at `v6.4.4.10685` | `crates/unarr-core/tests/movie_parser_vectors.rs` |

The replay maps every test method to the assertion the original makes and
fails on a method it does not know, so a fixture added upstream shows up as a
failure rather than being ignored. Methods about components Unarr ports
elsewhere, or not at all, are listed in the replay's `SKIPPED` tables.

Regenerate after bumping the pinned Sonarr release:

    git clone --depth 1 --branch v4.0.20.3014 https://github.com/Sonarr/Sonarr /tmp/sonarr
    scripts/port-vectors.py --sonarr /tmp/sonarr --out tests/vectors --area ParserTests
    scripts/port-vectors.py --sonarr /tmp/sonarr --out tests/vectors --area DecisionEngineTests

And after bumping the pinned Radarr release (its vectors live in a folder of
their own, since the fixtures carry the same names as Sonarr's):

    git clone --depth 1 --branch v6.4.4.10685 https://github.com/Radarr/Radarr /tmp/radarr
    scripts/port-vectors.py --source /tmp/radarr --out tests/vectors/radarr --area ParserTests
    scripts/port-vectors.py --source /tmp/radarr --out tests/vectors/radarr --area DecisionEngineTests
