# Indexer fixtures

Canned Newznab and Torznab answers for the mock indexers. Two servers serve
them with the same routing rules: `scripts/mock-servarr.py` while `make
golden` records from the real Sonarr, and the `unarr-mock` crate while the
tests drive Unarr. The rules are documented at the top of the script; in
short, the `t`, `tvdbid`, `rid`, `tvmazeid`, `q`, `season`, `ep` and `offset`
query parameters pick a file name, and a missing file answers `empty.xml`.

| Directory | Contents |
|-----------|----------|
| `newznab/` | `caps.xml`, `rss.xml` (what an RSS sync sees), `tvsearch-*.xml` and `search-*.xml` results, `nzb/` with NZB files by GUID (synthesised when absent). |
| `torznab/` | The same for a Torznab tracker, with `torznab:attr` seeders, peers and info hash; `torrent/` by GUID (synthesised when absent). |
| `movies/newznab/`, `movies/torznab/` | The same two indexers with movie feeds, for Radarr: served under `/movies/newznab` and `/movies/torznab`, with their own `caps.xml` (categories 2000; `movie-search` by `tmdbid` and `imdbid` on Newznab, by `imdbid` only on Torznab), `rss.xml`, `movie-tmdbid*.xml` and `movie-imdbid*.xml` results, and a `search-*.xml` that only the text fallback reaches. The items name releases of the movies of `tests/fixtures/radarr/`. Sonarr's feeds and caps are not touched by them. The releases meant to be approved weigh under 8 GB, above the minimum sizes of the fixture's quality definitions; one is 4 TB, larger than any disk the recording or the tests run on, so that the free-space rejection does not depend on the machine. |

The items name real-looking releases of the fixture's series (`tests/fixtures/sonarr/`),
so the goldens recorded for interactive search and RSS sync exercise the
parser, the mapping to series and episodes and the decision engine on data
that matches the imported library. Download links point at the mock on
`127.0.0.1:18989`, the port both servers use by default, since the provider
settings recorded in the goldens carry it.

Everything here is made up: no real indexer, GUID, URL or key appears.
