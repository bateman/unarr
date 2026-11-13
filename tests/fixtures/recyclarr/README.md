# Recyclarr replay fixtures

- `recyclarr.yml`: the maintainer's Recyclarr configuration (the `sonarr:`
  section, without `base_url` and `api_key`).
- `radarr.yml`: the `radarr:` section of the same configuration, as given
  on 11 November 2026, without `base_url` and `api_key`.
- `radarr-exchange.json`: the same four phases against the original Radarr
  6.4.4.10685 on the phase 4a fixture. Its `old-config` phase scores
  Radarr's FreeLeech (`0d91270a7255a1e388fa85e959f359d8`) in `HD Bluray +
  WEB`; `sync-1` deletes it again and leaves the `Language not Italian`
  the maintainer made by hand, which the configuration never names,
  untouched (`delete_old_custom_formats` removes only what Recyclarr
  managed).
- `sonarr-exchange.json`: what Recyclarr 8.8.1 and the original Sonarr
  4.0.20.3014 said to each other on the phase 1 fixture, in the four phases
  `scripts/recyclarr-replay.py` describes (an older configuration, a drift,
  two syncs), with the settings read back after each sync and Recyclarr's own
  output.

`make recyclarr` and `make recyclarr-radarr` send the recorded requests to a
running unarrd started on a fresh copy of that application's fixture and
compare the answers. They need neither Recyclarr nor the guide.

## Recording

Recyclarr 8.8.1 was used: Sonarr's exchange with a build from source
(`dotnet publish src/Recyclarr.Cli`, .NET 10 SDK), Radarr's with the
`recyclarr-linux-x64` release of the same tag (SHA-256
`960fe20cf2dd1c322ca7a6395469dcbefeaa7df08b217caad7686023b840feb9`). Both ran
with local resource providers only: a checkout of TRaSH-Guides at
`b8967a742411667577bf6cc6859fa9ae05e15549` and an empty templates directory.
The script writes the `settings.yml` for that, puts a recording proxy between
Recyclarr and the instance, and removes its working directory afterwards.

    scripts/sonarr-fixture.sh shell &
    scripts/recyclarr-replay.py --base-url http://127.0.0.1:8989 \
        --api-key 0123456789abcdef0123456789abcdef \
        --recyclarr 'dotnet /path/to/recyclarr.dll' --guide /path/to/TRaSH-Guides \
        --settle 6 --record tests/fixtures/recyclarr/sonarr-exchange.json

    scripts/radarr-fixture.sh shell &
    scripts/recyclarr-replay.py --app radarr --base-url http://127.0.0.1:7878 \
        --api-key 0123456789abcdef0123456789abcdef \
        --recyclarr /path/to/recyclarr --guide /path/to/TRaSH-Guides \
        --settle 6 --record tests/fixtures/recyclarr/radarr-exchange.json

The same command against unarrd, without `--record` and `--settle`, runs the
real Recyclarr against Unarr and compares the whole exchange, its output
included, with the recording.

The one answer that is not compared is `PUT qualitydefinition/update`: the
original answers it from a five-second cache (hence `--settle 6`), Unarr with
what it wrote.
