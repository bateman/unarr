# Releasing Unarr

How a version of Unarr is cut, what CI does with it, and what each image
tag means. Issue #11 holds what is still to settle before the first public
release (the user agent towards the Servarr servers, the repository made
public, the documentation for a user).

## Versions

Unarr's version follows semantic versioning, below 1.0 until the soak of
issue #10 is done: `0.MINOR.PATCH`, a minor for what is new or changes the
answers, a patch for fixes. It lives in one place, `[workspace.package]
version` in `Cargo.toml`, which every crate inherits and `system/status`
reports as `packageVersion`. The originals' versions, which `version`
reports, move with their pins (`scripts/*-fixture.sh`, issue #9), not with
Unarr's.

## Cutting a version

1. On `main`, with CI green on the commit to release.
2. In `CHANGELOG.md`, rename `## Unreleased` to `## 0.1.0 — 2026-12-01`
   (the version, a dash, the date) and start a new empty `## Unreleased`
   above it. Check the entry: `make release-notes VERSION=0.1.0`.
3. Set `version = "0.1.0"` in `Cargo.toml` and run `cargo check` so that
   `Cargo.lock` follows.
4. Commit both (`Release 0.1.0`), then tag that commit and push the tag:

       git tag -a v0.1.0 -m "Unarr 0.1.0"
       git push origin main v0.1.0

## What CI does with a tag

The workflow is `.github/workflows/ci.yml`, the same as for `main`:

1. `check`: first the tag itself, whose version must be the one in
   `Cargo.toml` and have an entry in `CHANGELOG.md`, or nothing is built and
   nothing published; then `make check` and `make panic-test`. Beside it,
   `deny`: `make deny`.
2. `image`: the image for linux/amd64 and linux/arm64, each built natively,
   with the OCI labels (source, licence, version, revision).
3. `manifest`: one multi-architecture image under the tags below. For a
   tag it waits for `deny`: a version's tags and `latest` are not
   published over an advisory, a build of `main` is.
4. `release`, for a tag only: the two static binaries are copied out of the
   images just published (`unarrd-linux-amd64`, `unarrd-linux-arm64`, the
   same bytes as in the image), with a `SHA256SUMS` file, and a GitHub
   Release named after the tag is created with them and the
   `CHANGELOG.md` entry as its notes. Each binary is checked to be of
   its architecture before it is published.

## Image tags

| Tag | Follows | For |
|---|---|---|
| `main` | every push to `main` | trying what is not released yet; the homelab runs it today |
| `latest` | the newest release | whoever wants releases only |
| `0.1.0` | that release, never moved | pinning one version; the way back after a bad upgrade |
| `0.1` | the newest patch of 0.1 | fixes only |

Before the first release, `latest` is the last build of `main` that set
it, and is no longer updated by pushes to `main`: point a deployment that
used it at `main`.

## Dependencies

`make deny` runs `cargo deny check` with `deny.toml`: every RustSec advisory
fails it, unmaintained and yanked crates included; every licence must be one
of those listed there, all of which can be combined with GPL-3.0; crates come
from crates.io only. An advisory that does not apply goes in `ignore` with its
reason, in the same commit as the reason. CI runs it on every push and pull
request, beside `check`.

`rust-version` in `Cargo.toml` is the oldest Rust that builds Unarr: the
highest of the dependencies' own floors (rusqlite 0.41 needs 1.89). Raising
a dependency can raise it; `cargo metadata` lists each crate's
`rust_version`. CI and the image build with the latest stable.
