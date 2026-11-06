# Unarr — project Makefile (bootstrap phase)
.DEFAULT_GOAL := help
NAME ?= unarr

.PHONY: help check-name logo

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  %-14s %s\n", $$1, $$2}'

check-name: ## Check a project name on crates.io, PyPI, npm, Docker Hub, brew, AUR, Repology, domains, GitHub (NAME=foo)
	@scripts/check-name.sh $(NAME)

logo: ## Render assets/brand/*.svg to PNG (needs node + playwright)
	@node scripts/render-logo.js
