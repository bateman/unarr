# Ported test vectors

Test cases extracted from the Sonarr repository (GPL-3.0, as Unarr) by
`scripts/port-vectors.py`, one JSON file per NUnit fixture:
`{"source": "<file in the Sonarr tree>", "methods": {"<test method>":
{"params": [...], "cases": [[arg, ...], ...]}}}`. Each method's arguments are
decoded C# literals; `Quality.X` and `Language.X` references are kept as
`{"enum": "Quality", "member": "X"}`.

| Directory | Source | Replayed by |
|-----------|--------|-------------|
| `parser/` | `src/NzbDrone.Core.Test/ParserTests/` at `v4.0.20.3014` | `crates/unarr-core/tests/parser_vectors.rs` |
| `decisionengine/` | `src/NzbDrone.Core.Test/DecisionEngineTests/` at `v4.0.20.3014` (the `TestCase`-driven fixtures; the others assert on mocks and are ported as unit tests in `crates/unarr-core/src/decision/`) | `crates/unarr-core/tests/decision_vectors.rs` |

The replay maps every test method to the assertion the original makes and
fails on a method it does not know, so a fixture added upstream shows up as a
failure rather than being ignored. Methods about components Unarr ports
elsewhere, or not at all, are listed in the replay's `SKIPPED` tables.

Regenerate after bumping the pinned Sonarr release:

    git clone --depth 1 --branch v4.0.20.3014 https://github.com/Sonarr/Sonarr /tmp/sonarr
    scripts/port-vectors.py --sonarr /tmp/sonarr --out tests/vectors --area ParserTests
    scripts/port-vectors.py --sonarr /tmp/sonarr --out tests/vectors --area DecisionEngineTests
