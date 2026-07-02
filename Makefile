.PHONY: help install sync dev pytest swift-build swift-test rust-build rust-test rust-lint rust-fmt test typecheck lint fmt codegen web-install web-typecheck web-build web-codegen-check web-ci audit licenses ci build package notarize clean serve rust-serve mac-app

PKG := helper
RUST := rust
FIXTURES := shared/fixtures/frames.jsonl
# Rust core serve: its own DB (separate migration system from the Python core's) + a stable port.
RUST_DB := sqlite://$(CURDIR)/outputs/db/hearsay-rust.db
RUST_PORT ?= 8799
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

HELPER_PRODUCTS := --product hearsay-helper --product hearsay-diarize --product hearsay-asr --product hearsay-live --product hearsay-me
swift-build: ## Build the Swift helper executables (explicit products skip FluidAudio's CLI, which has a type-check bug)
	swift build --package-path $(PKG) $(HELPER_PRODUCTS)

swift-test: ## Run the Swift cross-language self-test against the golden fixtures
	swift run --package-path $(PKG) hearsay-helper selftest $(FIXTURES)

rust-build: ## Build the Rust workspace
	cargo build --manifest-path $(RUST)/Cargo.toml

rust-test: ## Run the Rust workspace tests (cargo test)
	cargo test --manifest-path $(RUST)/Cargo.toml

rust-lint: ## Lint Rust (clippy with warnings denied + rustfmt --check)
	cargo clippy --manifest-path $(RUST)/Cargo.toml --all-targets -- -D warnings
	cargo fmt --manifest-path $(RUST)/Cargo.toml --all --check

rust-fmt: ## Format Rust (rustfmt)
	cargo fmt --manifest-path $(RUST)/Cargo.toml --all

test: swift-build pytest swift-test rust-test ## Run all tests (Python + Swift + Rust; build first so the integration test runs)

typecheck: ## Type-check (mypy --strict)
	uv run mypy src scripts

lint: rust-lint ## Lint (ruff + clippy + format checks)
	uv run ruff check
	uv run ruff format --check

fmt: rust-fmt ## Format (ruff + rustfmt)
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
	rm -rf $(PKG)/.build $(RUST)/target

serve: ## Serve the Python core (loopback API + WS + web UI)
	uv run hearsay serve

rust-serve: ## Serve the Rust core (SYNTHETIC=1 for no-permission plumbing; needs swift-build + web-build for a live run)
	@mkdir -p outputs/db
	HEARSAY_SERVER_PORT=$(RUST_PORT) DATABASE_URL="$(RUST_DB)" \
		cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-core $(if $(SYNTHETIC),-- --synthetic)

mac-app: ## Build the UNSIGNED macOS .app (Tauri shell + hearsay-core + Swift sidecars); needs `cargo install tauri-cli`
	cd web && npm run build
	$(MAKE) swift-build
	cargo build --manifest-path $(RUST)/Cargo.toml -p hearsay-core
	@mkdir -p web/src-tauri/binaries
	cp $(RUST)/target/debug/hearsay-core web/src-tauri/binaries/hearsay-core-aarch64-apple-darwin
	@for b in hearsay-helper hearsay-live hearsay-me hearsay-diarize; do \
		cp helper/.build/arm64-apple-macosx/debug/$$b web/src-tauri/binaries/$$b-aarch64-apple-darwin; \
	done
	cd web/src-tauri && cargo tauri build --bundles app
	@echo "built (unsigned/ad-hoc): web/src-tauri/target/release/bundle/macos/Hearsay.app"
