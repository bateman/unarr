# Metadata fixtures

Answers of the metadata hosts the originals call, served by both mocks
(`scripts/mock-servarr.py` to the real Sonarr while goldens are recorded,
`crates/unarr-mock` to Unarr in the tests) under `/<host><path>`. Public
data; nothing here is the maintainer's.

| Host | What | How it got here |
|------|------|-----------------|
| `skyhook.sonarr.tv` | `v1/tvdb/shows/en/<tvdbId>`: every series of the Sonarr fixture plus Breaking Bad (81189), Frieren (424536, anime) and The Pete Holmes Show (271728, daily); `v1/tvdb/search/en/<term>`: the lookups the goldens make; a 404 for 999999999 | Recorded with `RECORD_METADATA=1 make golden` (or `make sonarr`), November 2026, with Sonarr's user agent |
| `services.sonarr.tv` | `v1/scenemapping` (cut to the recorded series), `v1/dailyseries`, `v1/ping`, `v1/notification`, `v1/update/main` (the update check), `v1/update/main/changes` (the releases `GET /api/v3/update` lists); `v1/time` is answered live | Recorded, then `scripts/metadata-fixtures.py trim`; `changes` on 2026-11-22, with Sonarr 4.0.20.3014 asking |
| `thexem.info` | `map/havemap`, `map/allNames`, `map/all?id=424536` | Hand-made by `scripts/metadata-fixtures.py xem` in the shape of XEM's public API: XEM refuses data-centre networks, so it cannot be recorded from a cloud session. Frieren's tvdb episodes 15-28 are given as scene season 2. |
| `api.simkl.com` | `sync/activities`, `sync/all-items/shows/{plantowatch,hold,completed}` (the last one a 500) | Hand-made in the shape of Simkl's public API, with public titles and made-up Simkl ids: the maintainer's lists are never fetched |
| `api.radarr.video` | `v1/movie/<tmdbId>` (with credits), `v1/movie/imdb/<imdbId>`, `v1/search?q=&year=`, `v1/movie/collection/<tmdbId>`, `v1/list/imdb/<listId>`: the movies, lookups, collections and the list the Radarr goldens ask for; a 404 for 999999999 and tt9999999999. `v1/movie/changed` is one file whatever its `since=` | Recorded with `RECORD_METADATA=1 make golden-radarr`, November 2026, with Radarr's user agent; the IMDb list is hand-made with a made-up list id, the maintainer's watchlist is never fetched |
| `radarr.servarr.com` | `v1/ping`, `v1/notification`, `v1/update/master` (the update check), `v1/update/master/changes` (the releases `GET /api/v3/update` lists); `v1/time` is answered live | Recorded; `changes` on 2026-11-22, with Radarr 6.4.4.10685 asking |
| `image.tmdb.org` | every path: the stand-in poster, since TMDB's paths name no cover type | Generated; artwork is never fetched or committed |
| `artworks.thetvdb.com` | `artwork/*`: one stand-in image per cover type, at the originals' sizes | Generated; artwork is never fetched or committed |

A file's name is the path, then, when there is a query, a `/` and the
query slugged (`term=the pitt` gives `term-the-pitt`). The Servarr
services (`services.sonarr.tv`, `radarr.servarr.com`) are the exception:
their query only says which original asks (version, OS, runtime), so it is
left out and one answer serves every version, the next pin's included. A
`.status` file next to an answer holds a status other than 200.

The originals ask the Servarr services from their first request while a
recording runs (the update check, the notifications and the clock at
start, the recent releases for `GET /api/v3/update`), through the mock:
none of those answers comes from the real hosts during a recording. To
refresh one, with the hosts reachable, remove its file and record with
`RECORD_METADATA=1` (`upstream/README.md` has the commands).

To add a series or a search: run the recorder with `RECORD_METADATA=1`,
make the request, then `python3 -I scripts/metadata-fixtures.py trim` so
the scene mappings follow. To add a movie, a lookup or a collection: the
same with `make golden-radarr`; there is nothing to trim.
