# Unarr — project Makefile
.DEFAULT_GOAL := help
NAME  ?= unarr
IMAGE ?= ghcr.io/bateman/unarr
TAG   ?= dev

.PHONY: help build run run-both test fmt lint check clean docker check-name golden golden-radarr sonarr radarr ui ui-sonarr ui-radarr ui-present bazarr bazarr-radarr prowlarr prowlarr-radarr recyclarr recyclarr-radarr http-audit anonymize scale-fixture
UNARR_URL ?= http://127.0.0.1:8989
RADARR_URL ?= http://127.0.0.1:7878
UNARR_API_KEY ?= 0123456789abcdef0123456789abcdef
OUT ?= .cache/anonymized
APP ?= sonarr
# http-audit: Radarr unless AUDIT_APP=sonarr, which asks UNARR_URL and replays Sonarr's list.
AUDIT_APP ?= radarr
AUDIT_URL = $(if $(filter sonarr,$(AUDIT_APP)),$(UNARR_URL),$(RADARR_URL))
AUDIT_REQUESTS = $(or $(REQUESTS),tests/fixtures/$(AUDIT_APP)/golden/requests.txt)

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

scale-fixture: ## Grow the fixtures to 950 series and 2,123 movies with 50,000 history rows each in .cache/scaled, for measurements
	@python3 -I scripts/scale-fixture.py --out .cache/scaled/sonarr
	@python3 -I scripts/scale-fixture.py --app radarr --out .cache/scaled/radarr

compare-originals: ## Measure RAM, CPU and latency of the original Sonarr and Radarr beside Unarr (LIBRARY=real|scaled, REPEAT=n; about an hour and a half a run; needs make ui, and make scale-fixture for scaled)
	@python3 -I scripts/compare-originals.py --library $(or $(LIBRARY),real) --repeat $(or $(REPEAT),1)

bazarr: ## Replay Bazarr's requests against a running unarrd (UNARR_URL, UNARR_API_KEY)
	@python3 -I scripts/bazarr-replay.py --base-url $(UNARR_URL) --api-key $(UNARR_API_KEY)

bazarr-radarr: ## Replay Bazarr's movie requests against a running unarrd (RADARR_URL, UNARR_API_KEY)
	@python3 -I scripts/bazarr-replay.py --app radarr --base-url $(RADARR_URL) --api-key $(UNARR_API_KEY)

prowlarr: ## Replay Prowlarr's indexer sync against a running unarrd, with the mock indexer (UNARR_URL, UNARR_API_KEY)
	@python3 -I scripts/prowlarr-replay.py --base-url $(UNARR_URL) --api-key $(UNARR_API_KEY) --mock

prowlarr-radarr: ## Replay Prowlarr's indexer sync into Radarr against a running unarrd, with the mock indexer (RADARR_URL, UNARR_API_KEY)
	@python3 -I scripts/prowlarr-replay.py --app radarr --base-url $(RADARR_URL) --api-key $(UNARR_API_KEY) --mock

recyclarr: ## Replay the recorded Recyclarr sync into Sonarr (tests/fixtures/recyclarr) against a running unarrd on a fresh fixture (UNARR_URL, UNARR_API_KEY)
	@python3 -I scripts/recyclarr-replay.py --base-url $(UNARR_URL) --api-key $(UNARR_API_KEY)

recyclarr-radarr: ## Replay the recorded Recyclarr sync into Radarr against a running unarrd on a fresh fixture (RADARR_URL, UNARR_API_KEY)
	@python3 -I scripts/recyclarr-replay.py --app radarr --base-url $(RADARR_URL) --api-key $(UNARR_API_KEY)

http-audit: ## Compare status and headers of the original and of unarrd, both running on one fixture (ORIGINAL_URL, AUDIT_APP=radarr|sonarr, RADARR_URL or UNARR_URL, REQUESTS)
	@test -n "$(ORIGINAL_URL)" || { echo "usage: make http-audit ORIGINAL_URL=http://127.0.0.1:7880 [AUDIT_APP=sonarr] [RADARR_URL=...|UNARR_URL=...] [REQUESTS=...]"; exit 2; }
	@python3 -I scripts/http-audit.py --app $(AUDIT_APP) --original $(ORIGINAL_URL) --unarr $(AUDIT_URL) --requests $(AUDIT_REQUESTS)

anonymize: ## Anonymised copy of a Sonarr or Radarr backup zip or config dir (FROM=..., APP=sonarr, OUT=.cache/anonymized)
	@test -n "$(FROM)" || { echo "usage: make anonymize FROM=backup.zip [APP=radarr] [OUT=dir]"; exit 2; }
	@python3 scripts/arr-anonymize.py --app $(APP) --from "$(FROM)" --out "$(OUT)"

check-name: ## Check a project name on crates.io, PyPI, npm, Docker Hub, brew, AUR, Repology, domains, GitHub (NAME=foo)
	@scripts/check-name.sh $(NAME)
