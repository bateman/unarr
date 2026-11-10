# Recyclarr replay fixtures

- `recyclarr.yml`: the maintainer's Recyclarr configuration (the `sonarr:`
  section, without `base_url` and `api_key`).
- `sonarr-exchange.json`: what Recyclarr 8.8.1 and the original Sonarr
  4.0.20.3014 said to each other on the phase 1 fixture, in the four phases
  `scripts/recyclarr-replay.py` describes (an older configuration, a drift,
  two syncs), with the settings read back after each sync and Recyclarr's own
  output.

`make recyclarr` sends the recorded requests to a running unarrd started on a
fresh copy of the fixture and compares the answers. It needs neither
Recyclarr nor the guide.

## Recording

Recyclarr was built from source at tag `v8.8.1` (`dotnet publish
src/Recyclarr.Cli`, .NET 10 SDK) and run with local resource providers only:
a checkout of TRaSH-Guides at `b8967a742411667577bf6cc6859fa9ae05e15549` and
an empty templates directory. The script writes the `settings.yml` for that,
puts a recording proxy between Recyclarr and the instance, and removes its
working directory afterwards.

    scripts/sonarr-fixture.sh shell &
    scripts/recyclarr-replay.py --base-url http://127.0.0.1:8989 \
        --api-key 0123456789abcdef0123456789abcdef \
        --recyclarr 'dotnet /path/to/recyclarr.dll' --guide /path/to/TRaSH-Guides \
        --settle 6 --record tests/fixtures/recyclarr/sonarr-exchange.json

The same command against unarrd, without `--record` and `--settle`, runs the
real Recyclarr against Unarr and compares the whole exchange, its output
included, with the recording.

The one answer that is not compared is `PUT qualitydefinition/update`: the
original answers it from a five-second cache (hence `--settle 6`), Unarr with
what it wrote.
