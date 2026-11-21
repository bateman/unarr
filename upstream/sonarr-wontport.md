# Sonarr: upstream changes Unarr will not port

`scripts/upstream-triage.py` reads this list, so that what is here is not
assessed again. Each entry is a list item at the start of a line whose first
element is a code span, followed by the reason:

    - `<sha>` reason: one upstream commit (7 to 40 hex digits)
    - `<path glob>` reason: an upstream feature, by the paths it lives in

A commit matches a path glob when every path it touches matches one of the
globs; `*` also crosses `/`. A commit that adds or changes a migration is
always assessed (class `schema`), even when it matches: the importer must
know every migration of the databases it reads.

## Entries
