.PHONY: help install sync dev pytest swift-build swift-test test typecheck lint fmt codegen web-install web-typecheck web-build web-codegen-check web-ci audit licenses ci build package notarize clean serve

PKG := helper
FIXTURES := shared/fixtures/frames.jsonl
# Copyleft families we refuse (open-license gate; see CLAUDE.md).
DENY_LICENSES := GPL;AGPL;LGPL;MPL;EUPL;SSPL;CC-BY-SA

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

install sync: ## Sync the Python env (base + dev)
	uv sync

dev: ## Run the app for local development (placeholder until the core server lands)
	@echo "TODO: launch core + helper + web (Phase 1)"

pytest: ## Run Python tests
	uv run pytest -q

swift-build: ## Build the Swift helper
	swift build --package-path $(PKG)

swift-test: ## Run the Swift cross-language self-test against the golden fixtures
	swift run --package-path $(PKG) hearsay-helper selftest $(FIXTURES)

test: swift-build pytest swift-test ## Run all tests (Python + Swift; build first so the integration test runs)

typecheck: ## Type-check (mypy --strict)
	uv run mypy src scripts

lint: ## Lint (ruff)
	uv run ruff check

fmt: ## Format (ruff)
	uv run ruff format

codegen: ## Regenerate IPC fixtures + OpenAPI schema + web TS types
	uv run python scripts/gen_fixtures.py
	uv run python scripts/dump_openapi.py
	cd web && npm run codegen

web-install: ## Install pinned web deps (npm ci)
	cd web && npm ci

web-typecheck: ## Type-check the web UI (tsc)
	cd web && npm run typecheck

web-build: ## Build the web UI bundle (web/dist)
	cd web && npm run build

web-codegen-check: ## Fail if the OpenAPI schema / web TS types drift from source
	uv run python scripts/dump_openapi.py
	cd web && npm run codegen
	git diff --exit-code -- web/openapi.json web/src/api/schema.ts

web-ci: web-install web-codegen-check web-typecheck web-build ## Web CI gate (install, drift, typecheck, build)

audit: ## Dependency CVE scan (pip-audit)
	uv run pip-audit

licenses: ## Fail the build on copyleft dependency licenses (open-license gate)
	uv run pip-licenses --fail-on="$(DENY_LICENSES)" --format=markdown

ci: lint typecheck test audit licenses ## Full CI gate

build package notarize: ## Distribution targets (Phase 5; deferred for internal use)
	@echo "$@: deferred until there is a decision to distribute (see plan Phase 5)"

clean: ## Remove build artifacts
	rm -rf $(PKG)/.build

serve: ## Serve the application
	uv run hearsay serve
