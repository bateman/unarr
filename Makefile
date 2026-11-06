# Unarr — project Makefile
.DEFAULT_GOAL := help
NAME  ?= unarr
IMAGE ?= ghcr.io/bateman/unarr
TAG   ?= dev

.PHONY: help build run test fmt lint check clean docker check-name

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  %-12s %s\n", $$1, $$2}'

build: ## Build unarrd in release mode
	cargo build --release --bin unarrd

run: ## Run unarrd from source (RUST_LOG=debug for verbose output)
	cargo run --bin unarrd

test: ## Run all tests
	cargo test --workspace

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

check-name: ## Check a project name on crates.io, PyPI, npm, Docker Hub, brew, AUR, Repology, domains, GitHub (NAME=foo)
	@scripts/check-name.sh $(NAME)
