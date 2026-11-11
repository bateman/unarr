# Unarr — project Makefile
.DEFAULT_GOAL := help
NAME  ?= unarr
IMAGE ?= ghcr.io/bateman/unarr
TAG   ?= dev

.PHONY: help build run run-both test fmt lint check clean docker check-name golden golden-radarr sonarr radarr ui ui-sonarr ui-radarr ui-present bazarr bazarr-radarr prowlarr recyclarr anonymize scale-fixture
UNARR_URL ?= http://127.0.0.1:8989
RADARR_URL ?= http://127.0.0.1:7878
UNARR_API_KEY ?= 0123456789abcdef0123456789abcdef
OUT ?= .cache/anonymized
APP ?= sonarr

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  %-14s %s\n", $$1, $$2}'

build: ## Build unarrd in release mode
	cargo build --release --bin unarrd

run: ## Run unarrd from source on the Sonarr fixture (RUST_LOG=debug for verbose output)
	cargo run --bin unarrd -- --data .cache/unarr --import-from tests/fixtures/sonarr --bind 127.0.0.1

run-both: ## Run unarrd from source on both fixtures, Sonarr on 8989 and Radarr on 7878
	@for app in sonarr radarr; do mkdir -p .cache/unarr-both/$$app; \
		test -f .cache/unarr-both/$$app/config.xml || cp tests/fixtures/$$app/config.xml tests/fixtures/$$app/$$app.db .cache/unarr-both/$$app/; done
	cargo run --bin unarrd -- --data .cache/unarr-both --personalities sonarr,radarr --bind 127.0.0.1

test: ui-present ## Run all tests (needs the web UI: make ui)
	cargo test --workspace

ui-present:
	@for app in sonarr radarr; do test -f crates/unarr-api/ui/$$app/index.html || { echo "the $$app web UI is not embedded yet: run 'make ui' first (the login page tests need it)"; exit 1; }; done

fmt: ## Format the code
	cargo fmt --all

lint: ## Clippy with warnings as errors
	cargo clippy --workspace --all-targets -- -D warnings

check: ## fmt check + lint + test, what CI runs
	cargo fmt --all -- --check
	$(MAKE) lint
	$(MAKE) test

clean: ## Remove build artifacts
	cargo clean

docker: ## Build the container image locally ($(IMAGE):$(TAG))
	docker build -t $(IMAGE):$(TAG) .

golden: ## Re-record tests/fixtures/sonarr/golden from the real Sonarr on the fixture
	@scripts/sonarr-fixture.sh golden

sonarr: ## Run the real Sonarr on a copy of the fixture, for manual comparison
	@scripts/sonarr-fixture.sh shell

golden-radarr: ## Re-record tests/fixtures/radarr/golden from the real Radarr on the fixture
	@scripts/radarr-fixture.sh golden

radarr: ## Run the real Radarr on a copy of the fixture, for manual comparison
	@scripts/radarr-fixture.sh shell

ui: ui-sonarr ui-radarr ## Fetch the official web UIs into crates/unarr-api/ui for embedding

ui-sonarr: ## Fetch the official Sonarr web UI into crates/unarr-api/ui/sonarr
	@scripts/sonarr-fixture.sh ui

ui-radarr: ## Fetch the official Radarr web UI into crates/unarr-api/ui/radarr
	@scripts/radarr-fixture.sh ui

scale-fixture: ## Grow the fixture to 950 series and 50,000 history rows in .cache/scaled, for measurements
	@python3 -I scripts/scale-fixture.py --out .cache/scaled

bazarr: ## Replay Bazarr's requests against a running unarrd (UNARR_URL, UNARR_API_KEY)
	@python3 -I scripts/bazarr-replay.py --base-url $(UNARR_URL) --api-key $(UNARR_API_KEY)

bazarr-radarr: ## Replay Bazarr's movie requests against a running unarrd (RADARR_URL, UNARR_API_KEY)
	@python3 -I scripts/bazarr-replay.py --app radarr --base-url $(RADARR_URL) --api-key $(UNARR_API_KEY)

prowlarr: ## Replay Prowlarr's indexer sync against a running unarrd, with the mock indexer (UNARR_URL, UNARR_API_KEY)
	@python3 -I scripts/prowlarr-replay.py --base-url $(UNARR_URL) --api-key $(UNARR_API_KEY) --mock

recyclarr: ## Replay the recorded Recyclarr sync (tests/fixtures/recyclarr) against a running unarrd on a fresh fixture (UNARR_URL, UNARR_API_KEY)
	@python3 -I scripts/recyclarr-replay.py --base-url $(UNARR_URL) --api-key $(UNARR_API_KEY)

anonymize: ## Anonymised copy of a Sonarr or Radarr backup zip or config dir (FROM=..., APP=sonarr, OUT=.cache/anonymized)
	@test -n "$(FROM)" || { echo "usage: make anonymize FROM=backup.zip [APP=radarr] [OUT=dir]"; exit 2; }
	@python3 scripts/arr-anonymize.py --app $(APP) --from "$(FROM)" --out "$(OUT)"

check-name: ## Check a project name on crates.io, PyPI, npm, Docker Hub, brew, AUR, Repology, domains, GitHub (NAME=foo)
	@scripts/check-name.sh $(NAME)
