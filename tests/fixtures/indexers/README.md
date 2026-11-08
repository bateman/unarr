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

The items name real-looking releases of the fixture's series (`tests/fixtures/sonarr/`),
so the goldens recorded for interactive search and RSS sync exercise the
parser, the mapping to series and episodes and the decision engine on data
that matches the imported library. Download links point at the mock on
`127.0.0.1:18989`, the port both servers use by default, since the provider
settings recorded in the goldens carry it.

Everything here is made up: no real indexer, GUID, URL or key appears.
