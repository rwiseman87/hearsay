.DEFAULT_GOAL := help
SHELL := /bin/bash

.PHONY: help swift-plist-guard swift-build swift-test rust-build rust-test rust-lint tauri-lint tauri-test \
	test lint fmt codegen codegen-check web-install web-typecheck web-lint web-test web-build web-ci \
	audit licenses version-check version stamp-version set-version ci probes diarize-eval wer-eval \
	diarizer-eval robustness-eval live-eval aec-eval crash-eval eval coverage e2e test-all notarize clean \
	rust-serve stage-release mac-app dmg

PKG := helper
RUST := rust
WEB := web
TAURI := $(WEB)/src-tauri
OUT := outputs
RUST_MANIFEST := $(RUST)/Cargo.toml
TAURI_MANIFEST := $(TAURI)/Cargo.toml
PLIST := $(PKG)/Info.plist
FIXTURES := shared/fixtures/frames.jsonl
CONTROL_FIXTURES := shared/fixtures/control.jsonl
OPENAPI := $(WEB)/openapi.json
SCHEMA_TS := $(WEB)/src/api/schema.ts

# Cargo features: the core (echo cancellation) and the notes sidecar (Metal acceleration).
CORE_FEATURES := aec
NOTES_FEATURES := metal

# Rust core serve: its own DB (separate from any dev DB) + a stable port.
RUST_DB := sqlite://$(CURDIR)/$(OUT)/db/hearsay-rust.db
RUST_PORT ?= 8799

# The Swift executables: the capture helper plus the FluidAudio/ANE sidecars. `swift build` takes ONE
# `--product` (a repeated flag keeps only the last), so each product gets its own invocation.
SWIFT_PRODUCTS := hearsay-helper hearsay-diarize hearsay-live hearsay-me hearsay-models
SWIFT_CONFIG ?= debug

# Distribution staging: Tauri's `externalBin` expects `<name>-<target-triple>` under STAGE.
STAGE := $(TAURI)/binaries
TRIPLE := aarch64-apple-darwin
BUNDLE := $(TAURI)/target/release/bundle
APP := $(BUNDLE)/macos/Hearsay.app

# tauri-build validates every bundle input, and the staged sidecars + web/dist exist only after
# `make stage-release`; clippy and the tests bundle nothing, so null those inputs (merged config).
TAURI_NO_BUNDLE := TAURI_CONFIG='{"bundle":{"externalBin":null,"resources":null}}'

# rust/Cargo.toml is canonical. The Swift helper reads its version from the embedded Info.plist.
VERSION_TOML := $(RUST_MANIFEST) $(TAURI_MANIFEST)
VERSION_JSON := $(TAURI)/tauri.conf.json $(WEB)/package.json
VERSION_FILES := $(VERSION_TOML) $(VERSION_JSON) $(PLIST)
SED_I := sed -i ''

help: ## Show this help
	@awk 'BEGIN{FS=":.*## "; printf "Usage: make \033[36m<target>\033[0m\n"} \
		/^[a-zA-Z0-9_-]+:.*## /{printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2} \
		/^##@/{printf "\n\033[1m%s\033[0m\n", substr($$0, 5)}' $(MAKEFILE_LIST)

##@ Swift

# SwiftPM does not track the `-sectcreate` plist as a build input and hashes content, so a touch
# won't do: drop the stale binary to force the relink that re-embeds it.
swift-plist-guard:
	@for b in $(PKG)/.build/debug/hearsay-helper $(PKG)/.build/arm64-apple-macosx/release/hearsay-helper; do \
		if [ -f "$$b" ] && [ $(PLIST) -nt "$$b" ]; then \
			echo "Info.plist changed since $$b was linked; removing it to force a relink"; \
			rm -f "$$b"; \
		fi; \
	done

swift-build: swift-plist-guard ## Build the Swift executables, one invocation each (SWIFT_CONFIG=debug|release)
	@set -eu; for p in $(SWIFT_PRODUCTS); do \
		swift build -c $(SWIFT_CONFIG) --package-path $(PKG) --product $$p; \
	done

swift-test: ## Run the Swift cross-language self-test against the golden fixtures
	swift run --package-path $(PKG) hearsay-helper selftest $(FIXTURES)

##@ Rust

rust-build: ## Build the Rust workspace
	cargo build --manifest-path $(RUST_MANIFEST)

rust-test: ## Run the Rust workspace tests (cargo test)
	cargo test --manifest-path $(RUST_MANIFEST)

rust-lint: ## Lint Rust (clippy with warnings denied + rustfmt --check)
	cargo clippy --manifest-path $(RUST_MANIFEST) --all-targets -- -D warnings
	cargo fmt --manifest-path $(RUST_MANIFEST) --all --check

tauri-lint: ## Lint the Tauri shell (the shipping entrypoint; excluded from the rust/ workspace)
	$(TAURI_NO_BUNDLE) cargo clippy --manifest-path $(TAURI_MANIFEST) --all-targets -- -D warnings
	cargo fmt --manifest-path $(TAURI_MANIFEST) --check

tauri-test: ## Test the Tauri shell (cargo test on web/src-tauri; excluded from the rust/ workspace)
	$(TAURI_NO_BUNDLE) cargo test --manifest-path $(TAURI_MANIFEST)

fmt: ## Format Rust (rustfmt on the workspace and the Tauri shell)
	cargo fmt --manifest-path $(RUST_MANIFEST) --all
	cargo fmt --manifest-path $(TAURI_MANIFEST)

##@ Web

web-install: ## Install pinned web deps (npm ci)
	cd $(WEB) && npm ci

web-typecheck: ## Type-check the web UI (tsc)
	cd $(WEB) && npm run typecheck

web-lint: ## Lint the web UI (ESLint: typescript-eslint + react-hooks)
	cd $(WEB) && npm run lint

web-test: ## Web unit/component tests (vitest, jsdom); also folded into web-ci
	cd $(WEB) && npm run test

web-build: ## Build the web UI bundle (web/dist)
	cd $(WEB) && npm run build

web-ci: web-install web-typecheck web-lint web-test web-build ## Web CI gate (install, typecheck, lint, test, build)

##@ Codegen and versions

codegen: web-install ## Regenerate IPC fixtures + OpenAPI schema + web TS types (Rust is the source of truth)
	cargo run --manifest-path $(RUST_MANIFEST) -p hearsay-ipc --bin gen_fixtures
	cargo run --manifest-path $(RUST_MANIFEST) -p hearsay-core -- --dump-openapi > $(OPENAPI).tmp && mv $(OPENAPI).tmp $(OPENAPI)
	cd $(WEB) && npm run codegen

codegen-check: codegen ## Fail if the committed IPC fixtures / OpenAPI / web types drift from the Rust source
	git diff --exit-code -- $(FIXTURES) $(CONTROL_FIXTURES) $(OPENAPI) $(SCHEMA_TS)

# A placeholder in git; the release workflow stamps the real version from the release it is cutting.
version: ## Print the app version as committed (0.0.0 outside a release build)
	@grep -m1 '^version' $(RUST_MANIFEST) | sed -E 's/.*"(.*)".*/\1/'

# The release build's first step; no codegen, so it runs before `npm ci`.
stamp-version: ## Write the app version into all five files (usage: make stamp-version VERSION=0.2.0)
	@test -n "$(VERSION)" || { echo "usage: make stamp-version VERSION=x.y.z"; exit 1; }
	@echo "$(VERSION)" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$$' || \
		{ echo "ERROR: VERSION must be x.y.z (got '$(VERSION)')"; exit 1; }
	@set -eu; \
	for f in $(VERSION_TOML); do \
		$(SED_I) -E '1,/^version/ s/^version = ".*"/version = "$(VERSION)"/' "$$f"; \
	done; \
	for f in $(VERSION_JSON); do \
		$(SED_I) -E '1,/"version"/ s/("version"[[:space:]]*:[[:space:]]*)".*"/\1"$(VERSION)"/' "$$f"; \
	done; \
	$(SED_I) -E '/CFBundleShortVersionString/{n; s|<string>.*</string>|<string>$(VERSION)</string>|;}' $(PLIST)
	@$(MAKE) --no-print-directory version-check

# openapi.json embeds the version, so a change without codegen leaves codegen-check failing.
set-version: stamp-version ## Set the app version everywhere and regenerate codegen (usage: make set-version VERSION=0.2.0)
	@$(MAKE) --no-print-directory codegen

version-check: ## Fail if the app version drifts across the Rust workspace, Tauri shell, package.json, and helper plist
	@set -eu; \
	read_version() { \
		case "$$1" in \
			*.toml) grep -m1 '^version' "$$1" | sed -E 's/.*"(.*)".*/\1/' ;; \
			*.json) grep -m1 '"version"' "$$1" | sed -E 's/.*: *"(.*)".*/\1/' ;; \
			*.plist) /usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$$1" ;; \
		esac; \
	}; \
	canonical=$$(read_version $(RUST_MANIFEST)); \
	bad=0; \
	for f in $(VERSION_FILES); do \
		v=$$(read_version "$$f"); \
		if [ "$$v" != "$$canonical" ]; then echo "ERROR: version drift -- $$f is $$v, expected $$canonical"; bad=1; fi; \
	done; \
	if [ $$bad -ne 0 ]; then \
		echo "$(RUST_MANIFEST) is canonical; run 'make set-version VERSION=$$canonical' to align the rest."; \
		exit 1; \
	fi; \
	echo "version $$canonical consistent across $(words $(VERSION_FILES)) files"

##@ Gates

test: swift-build swift-test rust-test ## Run all tests (Swift + Rust)

lint: rust-lint tauri-lint ## Lint (clippy + rustfmt --check, both the workspace and the Tauri shell)

# --omit=dev: web dev deps are build-time only and never shipped, so audit only the production deps.
audit: ## Dependency CVE scan (cargo-audit over both Rust trees + npm audit)
	cd $(RUST) && cargo audit
	cd $(TAURI) && cargo audit
	cd $(WEB) && npm audit --omit=dev --audit-level=moderate

licenses: ## Fail the build on copyleft dependency licenses (cargo-deny; policy in rust/deny.toml)
	cargo deny --manifest-path $(RUST_MANIFEST) check licenses
	cargo deny --manifest-path $(TAURI_MANIFEST) --config $(RUST)/deny.toml check licenses

ci: lint test tauri-test web-ci codegen-check version-check audit licenses ## Full CI gate (Rust, Swift, Tauri, web, codegen drift, versions, supply chain)

##@ On-demand evals

# Nothing below is automatic (docs/testing.md): model/hardware probes, evals, coverage and e2e.
probes: ## Model/hardware tests (the #[ignore]d notes probes); needs the models + ANE/GPU
	cargo test --manifest-path $(RUST_MANIFEST) -p hearsay-notes --features $(NOTES_FEATURES) -- --ignored

diarize-eval: swift-build ## Diarization accuracy gate (speaker count + DER vs baseline); self-skips without audio/sidecar
	cargo test --manifest-path $(RUST_MANIFEST) -p hearsay-inference --test diarization_accuracy diarization_accuracy_gate -- --nocapture

wer-eval: swift-build ## Offline transcript gate (refine WER + cpWER vs baseline); self-skips without audio/sidecar
	cargo test --manifest-path $(RUST_MANIFEST) -p hearsay-eval --test asr_accuracy -- --nocapture

diarizer-eval: swift-build ## Report-only diarizer comparison over AMI ES2004a (slow; downloads models)
	HEARSAY_DIARIZER_EVAL=1 cargo test --manifest-path $(RUST_MANIFEST) -p hearsay-eval --test diarizer_compare -- --nocapture

live-eval: swift-build ## Live WER/cpWER gate + final-delay report at real-time pace (about 10 minutes)
	HEARSAY_LIVE_EVAL=1 cargo test --manifest-path $(RUST_MANIFEST) -p hearsay-eval --test live_eval -- --nocapture

robustness-eval: swift-build ## Refine robustness on local recordings (HEARSAY_ROBUSTNESS_DIR); counts only, never text
	HEARSAY_ROBUSTNESS_DIR=$(or $(HEARSAY_ROBUSTNESS_DIR),$(OUT)/recordings) cargo test --manifest-path $(RUST_MANIFEST) -p hearsay-eval --test robustness -- --nocapture

aec-eval: swift-build ## Report-only echo eval: Speex canceller + live pipeline on a synthetic echoed mic (needs AMI audio)
	HEARSAY_ECHO_EVAL=1 cargo test --manifest-path $(RUST_MANIFEST) -p hearsay-eval --features $(CORE_FEATURES) --test echo_eval -- --nocapture --test-threads=1

crash-eval: swift-build ## Opt-in crash recovery eval: SIGKILLs the real live sidecars mid-meeting and checks respawn and meeting times
	HEARSAY_CRASH_EVAL=1 cargo test --manifest-path $(RUST_MANIFEST) -p hearsay-eval --test crash_eval -- --nocapture --test-threads=1

eval: diarize-eval wer-eval live-eval ## Every gated accuracy/latency eval (diarization, offline transcript, live)

coverage: ## Coverage report (cargo-llvm-cov + vitest v8) into outputs/coverage/ (report-only)
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { echo "cargo-llvm-cov not installed: run 'cargo install cargo-llvm-cov'"; exit 1; }
	@mkdir -p $(OUT)/coverage/rust
	cargo llvm-cov clean --workspace --manifest-path $(RUST_MANIFEST)
	cargo llvm-cov --no-report --workspace --manifest-path $(RUST_MANIFEST)
	cargo llvm-cov report --lcov --output-path $(OUT)/coverage/rust/lcov.info --manifest-path $(RUST_MANIFEST)
	cargo llvm-cov report --html --output-dir $(OUT)/coverage/rust --manifest-path $(RUST_MANIFEST)
	cd $(WEB) && npm run coverage

e2e: ## Browser end-to-end (Playwright) vs the scripted core + vite; one-time: cd web && npm install && npx playwright install chromium
	@command -v npx >/dev/null 2>&1 || { echo "npx not found: install Node (https://nodejs.org)"; exit 1; }
	@test -d $(WEB)/node_modules/@playwright/test || { echo "playwright not installed: run 'cd web && npm install' (then 'npx playwright install chromium')"; exit 1; }
	@mkdir -p $(OUT)/e2e
# Build the core up front so Playwright's webServer starts fast; the scripted engine needs no features.
	cargo build --manifest-path $(RUST_MANIFEST) -p hearsay-core
# An archived (compressed-only) meeting recording for the playback spec, built by the real encoder.
	cargo run -q --manifest-path $(RUST_MANIFEST) -p hearsay-audio --example fixture -- $(OUT)/e2e/fixture
	cd $(WEB) && npx playwright test

test-all: ci probes e2e ## Run everything: the deterministic gate + the model/hardware probes + the browser E2E

##@ Run and package

# Built as a sibling sidecar so a llama.cpp crash cannot take down the core; the core has no notes feature.
rust-serve: ## Serve the Rust core (SYNTHETIC=1 for no-permission plumbing; needs swift-build + web-build for a live run)
	@mkdir -p $(OUT)/db
	cargo build --manifest-path $(RUST_MANIFEST) -p hearsay-notes --features $(NOTES_FEATURES)
	HEARSAY_SERVER_PORT=$(RUST_PORT) DATABASE_URL="$(RUST_DB)" \
		cargo run --manifest-path $(RUST_MANIFEST) -p hearsay-core --features $(CORE_FEATURES),api-console $(if $(SYNTHETIC),-- --synthetic)

# Release binaries + web bundle, copied where Tauri's `externalBin` expects them. Shared by mac-app and dmg.
stage-release: version-check web-install web-build ## Build release binaries + web bundle and stage them for the Tauri bundle
	@test "$$(uname -m)" = "arm64" || { echo "stage-release: Apple-Silicon (arm64) only (got $$(uname -m)); the bundle is Apple-Silicon only"; exit 1; }
	@$(MAKE) --no-print-directory swift-build SWIFT_CONFIG=release
# The notes LLM ships as its own `hearsay-notes` sidecar (Metal), so the core is built without notes.
	cargo build --release --manifest-path $(RUST_MANIFEST) -p hearsay-core --features $(CORE_FEATURES)
	cargo build --release --manifest-path $(RUST_MANIFEST) -p hearsay-notes --features $(NOTES_FEATURES)
	@mkdir -p $(STAGE)
	cp $(RUST)/target/release/hearsay-core $(STAGE)/hearsay-core-$(TRIPLE)
	cp $(RUST)/target/release/hearsay-notes $(STAGE)/hearsay-notes-$(TRIPLE)
	@set -euo pipefail; for b in $(SWIFT_PRODUCTS); do \
		cp $(PKG)/.build/arm64-apple-macosx/release/$$b $(STAGE)/$$b-$(TRIPLE); \
	done

mac-app: stage-release ## Build the UNSIGNED .app (ad-hoc signed; core + sidecars); needs `cargo install tauri-cli`
	cd $(TAURI) && cargo tauri build --bundles app
	codesign --verify --deep --strict --verbose=2 $(APP)
	@echo "built (unsigned/ad-hoc): $(APP)"

# Depends on mac-app: the dmg bundler consumes the .app, so it is signed + verified first.
dmg: mac-app ## Build the distributable UNSIGNED .dmg (ad-hoc signed, no notarization)
# CI=true skips bundle_dmg.sh's Finder AppleScript styling (needs a GUI session); only icon positions are lost.
	cd $(TAURI) && CI=true cargo tauri build --bundles dmg
	@echo "built (unsigned/ad-hoc): $(BUNDLE)/dmg/ (see docs/packaging.md)"
	@echo "install on another Mac: drag to /Applications, then run"
	@echo "  xattr -dr com.apple.quarantine /Applications/Hearsay.app"

notarize: ## Not implemented: prints that notarization needs a paid Apple Developer ID
	@echo "$@: needs an Apple Developer ID + notarization; use 'make dmg' for the unsigned build"

##@ Clean

clean: ## Remove build artifacts, staged binaries, and the coverage + e2e report dirs
	rm -rf $(OUT)/coverage $(OUT)/e2e $(PKG)/.build $(RUST)/target $(WEB)/dist $(WEB)/node_modules $(TAURI)/target $(STAGE)
