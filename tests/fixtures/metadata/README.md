# Metadata fixtures

Answers of the metadata hosts the originals call, served by both mocks
(`scripts/mock-servarr.py` to the real Sonarr while goldens are recorded,
`crates/unarr-mock` to Unarr in the tests) under `/<host><path>`. Public
data; nothing here is the maintainer's.

| Host | What | How it got here |
|------|------|-----------------|
| `skyhook.sonarr.tv` | `v1/tvdb/shows/en/<tvdbId>`: every series of the Sonarr fixture plus Breaking Bad (81189), Frieren (424536, anime) and The Pete Holmes Show (271728, daily); `v1/tvdb/search/en/<term>`: the lookups the goldens make; a 404 for 999999999 | Recorded with `RECORD_METADATA=1 make golden` (or `make sonarr`), November 2026, with Sonarr's user agent |
| `services.sonarr.tv` | `v1/scenemapping` (cut to the recorded series), `v1/dailyseries`, `v1/ping`, `v1/notification`, `v1/update`; `v1/time` is answered live | Recorded, then `scripts/metadata-fixtures.py trim` |
| `thexem.info` | `map/havemap`, `map/allNames`, `map/all?id=424536` | Hand-made by `scripts/metadata-fixtures.py xem` in the shape of XEM's public API: XEM refuses data-centre networks, so it cannot be recorded from a cloud session. Frieren's tvdb episodes 15-28 are given as scene season 2. |
| `api.simkl.com` | `sync/activities`, `sync/all-items/shows/{plantowatch,hold,completed}` (the last one a 500) | Hand-made in the shape of Simkl's public API, with public titles and made-up Simkl ids: the maintainer's lists are never fetched |
| `artworks.thetvdb.com` | `artwork/*`: one stand-in image per cover type, at the originals' sizes | Generated; artwork is never fetched or committed |

A file's name is the path, then, when there is a query, a `/` and the
query slugged (`term=the pitt` gives `term-the-pitt`). A `.status` file
next to an answer holds a status other than 200.

To add a series or a search: run the recorder with `RECORD_METADATA=1`,
make the request, then `python3 -I scripts/metadata-fixtures.py trim` so
the scene mappings follow.
