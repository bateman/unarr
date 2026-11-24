# Unarr — project Makefile
.DEFAULT_GOAL := help
NAME  ?= unarr
IMAGE ?= ghcr.io/bateman/unarr
TAG   ?= dev

.PHONY: help build run run-both test panic-test fmt lint check deny release-notes clean docker check-name golden golden-radarr sonarr radarr ui ui-sonarr ui-radarr ui-present bazarr bazarr-radarr prowlarr prowlarr-radarr recyclarr recyclarr-radarr http-audit anonymize scale-fixture compare-originals route-percentiles test-scripts upstream-sonarr upstream-radarr bump-sonarr bump-radarr ui-smoke \
	upstream-plan-sonarr upstream-plan-radarr upstream-announce-sonarr upstream-announce-radarr \
	upstream-assemble-sonarr upstream-assemble-radarr upstream-pr-sonarr upstream-pr-radarr \
	upstream-apply-sonarr upstream-apply-radarr
UNARR_URL ?= http://127.0.0.1:8989
RADARR_URL ?= http://127.0.0.1:7878
UNARR_API_KEY ?= 0123456789abcdef0123456789abcdef
OUT ?= .cache/anonymized
APP ?= sonarr
# http-audit: Radarr unless AUDIT_APP=sonarr, which asks UNARR_URL and replays Sonarr's list.
AUDIT_APP ?= radarr
AUDIT_URL = $(if $(filter sonarr,$(AUDIT_APP)),$(UNARR_URL),$(RADARR_URL))
AUDIT_REQUESTS = $(or $(REQUESTS),tests/fixtures/$(AUDIT_APP)/golden/requests.txt)
# ui-smoke: the personalities to open, and where its Python packages live.
SMOKE_APP ?= sonarr,radarr
UI_SMOKE_VENV ?= $(or $(UNARR_CACHE),.cache)/ui-smoke-venv

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

panic-test: ui-present ## What a panic costs, on a release build with the test-only panic switch (in target/panic-switch: target/release/unarrd never has the switch)
	cargo test --release --target-dir target/panic-switch -p unarrd --features panic-switch --test panic

ui-present:
	@for app in sonarr radarr; do test -f crates/unarr-api/ui/$$app/index.html || { echo "the $$app web UI is not embedded yet: run 'make ui' first (the login page tests need it)"; exit 1; }; done

fmt: ## Format the code
	cargo fmt --all

lint: ## Clippy with warnings as errors
	cargo clippy --workspace --all-targets -- -D warnings

check: ## fmt check + lint + test + the scripts' tests, what CI runs
	cargo fmt --all -- --check
	$(MAKE) lint
	$(MAKE) test
	$(MAKE) test-scripts

ui-smoke: ui-present $(UI_SMOKE_VENV)/.installed ## Open every page of the embedded UIs in a headless browser over unarrd on a copy of the fixtures; fails on console errors, 4xx/5xx, pages left loading (SMOKE_APP=sonarr|radarr, UI_SMOKE_CHANNEL=chrome; report in .cache/ui-smoke)
	@$(UI_SMOKE_VENV)/bin/python -I scripts/ui-smoke.py --app $(SMOKE_APP)

$(UI_SMOKE_VENV)/.installed: scripts/ui-smoke-requirements.txt
	python3 -m venv $(UI_SMOKE_VENV)
	$(UI_SMOKE_VENV)/bin/pip install -q -r $<
	@touch $@

test-scripts: ## Unit tests of the Python scripts (scripts/tests; no network)
	@python3 -I -m unittest discover -s scripts/tests -t scripts/tests

deny: ## Advisories, licences and sources of the dependencies (cargo install cargo-deny; deny.toml)
	cargo deny check

release-notes: ## The CHANGELOG.md entry of a version, as its GitHub Release says it (VERSION=0.1.0)
	@test -n "$(VERSION)" || { echo "usage: make release-notes VERSION=0.1.0"; exit 2; }
	@python3 -I scripts/release-notes.py $(VERSION)

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

compare-originals: ## Measure RAM, CPU and latency of the original Sonarr and Radarr beside Unarr (LIBRARY=real|scaled, REPEAT=n; about an hour a run; needs make ui, and make scale-fixture for scaled)
	@python3 -I scripts/compare-originals.py --library $(or $(LIBRARY),real) --repeat $(or $(REPEAT),1)

route-percentiles: ## p50, p95, p99 of the routes with a target in docs/DESIGN.md section 9, originals beside Unarr (LIBRARY=real|scaled; builds unarrd first; needs make ui, and make scale-fixture for scaled)
	cargo build --release --bin unarrd
	@python3 -I scripts/route-percentiles.py --library $(or $(LIBRARY),real)

upstream-sonarr: ## Fetch upstream Sonarr, triage its commits since upstream/sonarr.lock up to the target release (TARGET=X.Y.Z.BUILD, FORMAT=json, RELEASES=api|tags|file)
	@python3 -I scripts/upstream-triage.py --app sonarr --fetch --releases $(or $(RELEASES),api) $(if $(TARGET),--target $(TARGET)) $(if $(FORMAT),--format $(FORMAT))

upstream-radarr: ## Fetch upstream Radarr, triage its commits since upstream/radarr.lock up to the target release (TARGET=X.Y.Z.BUILD, FORMAT=json, RELEASES=api|tags|file)
	@python3 -I scripts/upstream-triage.py --app radarr --fetch --releases $(or $(RELEASES),api) $(if $(TARGET),--target $(TARGET)) $(if $(FORMAT),--format $(FORMAT))

# bump-<app>: the arguments of scripts/pin-bump.py, from the variables the help names.
BUMP_ARGS = --releases $(or $(RELEASES),api) $(if $(TARGET),--target $(TARGET)) $(if $(STEPS),--steps $(STEPS)) \
	$(if $(DRY_RUN),--dry-run) $(if $(BASELINE),--baseline) $(if $(LISTS),--lists $(LISTS))

bump-sonarr: ## Bump upstream/sonarr.pin to the triage's target and record again from it; summary in .cache/bump/sonarr (TARGET=, RELEASES=api|tags|file, STEPS=fetch,record,apply,test,summary or cursor at the end of the port, DRY_RUN=1, BASELINE=1, LISTS=)
	@python3 -I scripts/pin-bump.py --app sonarr $(BUMP_ARGS)

bump-radarr: ## Bump upstream/radarr.pin to the triage's target and record again from it; summary in .cache/bump/radarr (TARGET=, RELEASES=api|tags|file, STEPS=fetch,record,apply,test,summary or cursor at the end of the port, DRY_RUN=1, BASELINE=1, LISTS=)
	@python3 -I scripts/pin-bump.py --app radarr $(BUMP_ARGS)

# The workflow `upstream` (.github/workflows/upstream.yml; upstream/README.md, "The workflow: up to the
# draft PR"). UPSTREAM_SCHEDULED: the personalities its weekly run follows; a manual start runs any.
# The target and the event come from the environment (TARGET, EVENT) and reach the script quoted.
UPSTREAM_SCHEDULED ?= radarr
UPSTREAM_OUT = $(or $(UNARR_CACHE),.cache)/upstream-workflow
UPSTREAM_WORK = $(or $(UNARR_CACHE),.cache)/bump
UPSTREAM_WORKFLOW = python3 -I scripts/upstream-workflow.py
UPSTREAM_PLAN = plan --event "$${EVENT:-workflow_dispatch}" --target "$$TARGET" --releases $(or $(RELEASES),api) \
	--scheduled "$(UPSTREAM_SCHEDULED)"

upstream-plan-sonarr: ## What the workflow does for Sonarr: the target, the jump, nothing when no release is new or a port is open (EVENT=schedule|workflow_dispatch, TARGET=, RELEASES=); plan.json in .cache/upstream-workflow/sonarr
	@$(UPSTREAM_WORKFLOW) $(UPSTREAM_PLAN) --app sonarr --out $(UPSTREAM_OUT)/sonarr

upstream-plan-radarr: ## What the workflow does for Radarr: the target, the jump, nothing when no release is new or a port is open (EVENT=schedule|workflow_dispatch, TARGET=, RELEASES=); plan.json in .cache/upstream-workflow/radarr
	@$(UPSTREAM_WORKFLOW) $(UPSTREAM_PLAN) --app radarr --out $(UPSTREAM_OUT)/radarr

upstream-announce-sonarr: ## Open the issue "Sonarr <version> is out" of the plan's new major, unless it is open (the workflow, GITHUB_TOKEN)
	@$(UPSTREAM_WORKFLOW) announce --app sonarr --plan $(UPSTREAM_OUT)/sonarr/plan.json

upstream-announce-radarr: ## Open the issue "Radarr <version> is out" of the plan's new major, unless it is open (the workflow, GITHUB_TOKEN)
	@$(UPSTREAM_WORKFLOW) announce --app radarr --plan $(UPSTREAM_OUT)/radarr/plan.json

upstream-assemble-sonarr: ## The bump's work directory from the workflow's artifacts: the fetched files over the recorded ones, and the checked test results when there are
	@$(UPSTREAM_WORKFLOW) assemble --trusted $(UPSTREAM_OUT)/sonarr/fetched --untrusted $(UPSTREAM_OUT)/sonarr/recorded --work $(UPSTREAM_WORK)/sonarr \
		$(if $(wildcard $(UPSTREAM_OUT)/sonarr/tested),--tested $(UPSTREAM_OUT)/sonarr/tested)

upstream-apply-sonarr: ## The bump of Sonarr applied in the workflow's job that writes; the extracted new original removed at once, never run there
	@$(MAKE) --no-print-directory bump-sonarr STEPS=apply
	rm -rf $(or $(UNARR_CACHE),.cache/sonarr)/Sonarr

upstream-assemble-radarr: ## The bump's work directory from the workflow's artifacts: the fetched files over the recorded ones, and the checked test results when there are
	@$(UPSTREAM_WORKFLOW) assemble --trusted $(UPSTREAM_OUT)/radarr/fetched --untrusted $(UPSTREAM_OUT)/radarr/recorded --work $(UPSTREAM_WORK)/radarr \
		$(if $(wildcard $(UPSTREAM_OUT)/radarr/tested),--tested $(UPSTREAM_OUT)/radarr/tested)

upstream-apply-radarr: ## The bump of Radarr applied in the workflow's job that writes; the extracted new original removed at once, never run there
	@$(MAKE) --no-print-directory bump-radarr STEPS=apply
	rm -rf $(or $(UNARR_CACHE),.cache/radarr)/Radarr

upstream-pr-sonarr: ## Commit the applied bump of Sonarr, push sonarr/upstream-<target>, open the draft PR (the workflow, UPSTREAM_TOKEN or GITHUB_TOKEN)
	@$(UPSTREAM_WORKFLOW) pr --app sonarr --plan $(UPSTREAM_OUT)/sonarr/plan.json --work $(UPSTREAM_WORK)/sonarr --base "$${BASE:-main}"

upstream-pr-radarr: ## Commit the applied bump of Radarr, push radarr/upstream-<target>, open the draft PR (the workflow, UPSTREAM_TOKEN or GITHUB_TOKEN)
	@$(UPSTREAM_WORKFLOW) pr --app radarr --plan $(UPSTREAM_OUT)/radarr/plan.json --work $(UPSTREAM_WORK)/radarr --base "$${BASE:-main}"

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

# The workflows that follow upstream (issue #9): .github/workflows/upstream-checks.yml
# and upstream-port.yml call these. Their values come from the environment and are
# read by the shell ($$NAME), never expanded by make: a workflow input is data.
.PHONY: upstream-checks-plan upstream-checks-run upstream-checks-apply upstream-port-plan upstream-main-protected

upstream-main-protected: ## Refuse unless a ruleset on main asks for an approved pull request that the token GH_TOKEN cannot bypass (GITHUB_REPOSITORY)
	@python3 -I scripts/upstream-checks.py main-protected

upstream-checks-plan: ## The port's PR and the checks asked for, as a job's outputs (PR_JSON from gh pr view, PR, RECORDINGS, HTTP_AUDIT, COMPARE, REPLAYS, OLD_PIN_REQUEST, OLD_PIN_WAIT)
	@python3 -I scripts/upstream-checks.py plan --pr-json "$$PR_JSON" --pr "$$PR" --repository "$$GITHUB_REPOSITORY" \
		--recordings "$$RECORDINGS" --http-audit "$$HTTP_AUDIT" --compare "$${COMPARE:-none}" --replays "$$REPLAYS" \
		--old-pin-request "$$OLD_PIN_REQUEST" --old-pin-wait "$${OLD_PIN_WAIT:-0}"

upstream-checks-run: ## Run the checks that need the original, on the checkout WORK (default this one; CHECK_APP, CHECKS from upstream-checks-plan, CHECKS_OUT; needs make ui and make build there)
	@python3 -I scripts/upstream-checks.py run --repo "$${WORK:-.}" --app "$$CHECK_APP" --checks "$$CHECKS" \
		--out "$${CHECKS_OUT:-.cache/upstream-checks/out}"

upstream-checks-apply: ## Check the checks' artifact, commit what may be committed in WORK, write the PR comment (CHECK_APP, ARTIFACT, WORK, PR, HEAD_SHA, COMMENT, RUN_URL)
	@python3 -I scripts/upstream-checks.py apply --artifact "$$ARTIFACT" --repo "$$WORK" --app "$$CHECK_APP" \
		--pr "$$PR" --head "$$HEAD_SHA" --comment "$$COMMENT" --run-url "$$RUN_URL"

upstream-port-plan: ## The port's draft PR, the run's limits and the agent's prompt, as a job's outputs (PR_JSON, PR, HELPER, GITHUB_OUTPUT)
	@python3 -I scripts/upstream-port.py plan --pr-json "$$PR_JSON" --pr "$$PR" --repository "$$GITHUB_REPOSITORY" \
		--helper "$$HELPER" $${GITHUB_OUTPUT:+--github-output "$$GITHUB_OUTPUT"}
