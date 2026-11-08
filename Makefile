# Unarr — project Makefile
.DEFAULT_GOAL := help
NAME  ?= unarr
IMAGE ?= ghcr.io/bateman/unarr
TAG   ?= dev

.PHONY: help build run test fmt lint check clean docker check-name golden sonarr ui ui-present bazarr prowlarr anonymize scale-fixture
UNARR_URL ?= http://127.0.0.1:8989
UNARR_API_KEY ?= 0123456789abcdef0123456789abcdef
OUT ?= .cache/anonymized

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  %-12s %s\n", $$1, $$2}'

build: ## Build unarrd in release mode
	cargo build --release --bin unarrd

run: ## Run unarrd from source on the Sonarr fixture (RUST_LOG=debug for verbose output)
	cargo run --bin unarrd -- --data .cache/unarr --import-from tests/fixtures/sonarr --bind 127.0.0.1

test: ui-present ## Run all tests (needs the web UI: make ui)
	cargo test --workspace

ui-present:
	@test -f crates/unarr-api/ui/sonarr/index.html || { echo "the web UI is not embedded yet: run 'make ui' first (the login page tests need it)"; exit 1; }

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

ui: ## Fetch the official Sonarr web UI into crates/unarr-api/ui/sonarr for embedding
	@scripts/sonarr-fixture.sh ui

scale-fixture: ## Grow the fixture to 950 series and 50,000 history rows in .cache/scaled, for measurements
	@python3 -I scripts/scale-fixture.py --out .cache/scaled

bazarr: ## Replay Bazarr's requests against a running unarrd (UNARR_URL, UNARR_API_KEY)
	@python3 scripts/bazarr-replay.py --base-url $(UNARR_URL) --api-key $(UNARR_API_KEY)

prowlarr: ## Replay Prowlarr's indexer sync against a running unarrd, with the mock indexer (UNARR_URL, UNARR_API_KEY)
	@python3 -I scripts/prowlarr-replay.py --base-url $(UNARR_URL) --api-key $(UNARR_API_KEY) --mock

anonymize: ## Anonymised copy of a Sonarr backup zip or config dir (FROM=..., OUT=.cache/anonymized)
	@test -n "$(FROM)" || { echo "usage: make anonymize FROM=sonarr_backup.zip [OUT=dir]"; exit 2; }
	@python3 scripts/sonarr-anonymize.py --from "$(FROM)" --out "$(OUT)"

check-name: ## Check a project name on crates.io, PyPI, npm, Docker Hub, brew, AUR, Repology, domains, GitHub (NAME=foo)
	@scripts/check-name.sh $(NAME)
