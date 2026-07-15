.PHONY: help swift-build swift-test rust-build rust-test rust-lint tauri-lint rust-fmt test lint fmt codegen codegen-check web-install web-typecheck web-lint web-build web-ci audit licenses version-check ci build package notarize clean serve rust-serve stage-model fetch-fluid-models stage-fluid-models stage-release mac-app dmg

PKG := helper
RUST := rust
FIXTURES := shared/fixtures/frames.jsonl
# Rust core serve: its own DB (separate from any dev DB) + a stable port.
RUST_DB := sqlite://$(CURDIR)/outputs/db/hearsay-rust.db
RUST_PORT ?= 8799

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

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

tauri-lint: ## Lint the Tauri shell (the shipping entrypoint; excluded from the rust/ workspace)
	cargo clippy --manifest-path web/src-tauri/Cargo.toml --all-targets -- -D warnings
	cargo fmt --manifest-path web/src-tauri/Cargo.toml --check

rust-fmt: ## Format Rust (rustfmt)
	cargo fmt --manifest-path $(RUST)/Cargo.toml --all

test: swift-build swift-test rust-test ## Run all tests (Swift + Rust)

lint: rust-lint tauri-lint ## Lint (clippy + rustfmt --check, both the workspace and the Tauri shell)

fmt: rust-fmt ## Format (rustfmt)

codegen: ## Regenerate IPC fixtures + OpenAPI schema + web TS types (Rust is the source of truth)
	cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-ipc --bin gen_fixtures
	cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-core -- --dump-openapi > web/openapi.json
	cd web && npm run codegen

codegen-check: ## Fail if the committed IPC fixtures / OpenAPI / web types drift from the Rust source
	cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-ipc --bin gen_fixtures
	cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-core -- --dump-openapi > web/openapi.json
	cd web && npm run codegen
	git diff --exit-code -- $(FIXTURES) web/openapi.json web/src/api/schema.ts

web-install: ## Install pinned web deps (npm ci)
	cd web && npm ci

web-typecheck: ## Type-check the web UI (tsc)
	cd web && npm run typecheck

web-lint: ## Lint the web UI (ESLint: typescript-eslint + react-hooks)
	cd web && npm run lint

web-build: ## Build the web UI bundle (web/dist)
	cd web && npm run build

web-ci: web-install web-typecheck web-lint web-build ## Web CI gate (install, typecheck, lint, build)

audit: ## Dependency CVE scan (cargo-audit over both Rust trees + npm audit)
	cd $(RUST) && cargo audit
	cd web/src-tauri && cargo audit
	cd web && npm audit --audit-level=moderate

licenses: ## Fail the build on copyleft dependency licenses (cargo-deny; policy in rust/deny.toml)
	cargo deny --manifest-path $(RUST)/Cargo.toml check licenses
	cargo deny --manifest-path web/src-tauri/Cargo.toml --config rust/deny.toml check licenses

version-check: ## Fail if the app version drifts across the Rust workspace, Tauri config, and package.json
	@rust=$$(grep -m1 '^version' $(RUST)/Cargo.toml | sed -E 's/.*"(.*)".*/\1/'); \
	tauri=$$(grep -m1 '"version"' web/src-tauri/tauri.conf.json | sed -E 's/.*: *"(.*)".*/\1/'); \
	pkg=$$(grep -m1 '"version"' web/package.json | sed -E 's/.*: *"(.*)".*/\1/'); \
	if [ "$$rust" != "$$tauri" ] || [ "$$rust" != "$$pkg" ]; then \
		echo "ERROR: version drift (rust/Cargo.toml=$$rust tauri.conf.json=$$tauri package.json=$$pkg)"; \
		echo "rust/Cargo.toml is canonical; update the other two to match."; \
		exit 1; \
	fi; \
	echo "version $$rust consistent across rust/Cargo.toml, tauri.conf.json, package.json"

ci: lint test codegen-check version-check audit licenses web-ci ## Full CI gate (Rust + Swift + Tauri + web + codegen drift + versions + supply-chain)

build notarize: ## Notarized distribution (needs an Apple Developer account; out of scope)
	@echo "$@: needs an Apple Developer ID + notarization; use 'make dmg' for the unsigned build"

package: dmg ## Build the distributable DMG (alias for 'dmg')

clean: ## Remove build artifacts (Swift, Rust, web bundle + deps, Tauri target, staged binaries/models)
	rm -rf $(PKG)/.build $(RUST)/target web/dist web/node_modules web/src-tauri/target $(STAGE) $(dir $(MODEL_DST))

serve rust-serve: ## Serve the Rust core (SYNTHETIC=1 for no-permission plumbing; needs swift-build + web-build for a live run)
	@mkdir -p outputs/db
	HEARSAY_SERVER_PORT=$(RUST_PORT) DATABASE_URL="$(RUST_DB)" \
		cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-core --features metal $(if $(SYNTHETIC),-- --synthetic)

# Distribution staging: build RELEASE binaries + web bundle, then copy them where Tauri's
# `externalBin` expects them (`<name>-<target-triple>`). Shared by `mac-app` and `dmg`.
STAGE := web/src-tauri/binaries
SIDECARS := hearsay-helper hearsay-live hearsay-me hearsay-diarize
APP := web/src-tauri/target/release/bundle/macos/Hearsay.app
# Whisper GGML model bundled for the offline refine (Tauri resource -> Contents/Resources/models/).
REFINE_MODEL := ggml-large-v3-turbo.bin
MODEL_SRC := outputs/models/$(REFINE_MODEL)
MODEL_DST := web/src-tauri/models/$(REFINE_MODEL)

# FluidAudio live models bundled so the installer is self-contained (no first-run HuggingFace
# download). Only the repos the shipped sidecars actually load: batch Parakeet ASR + streaming
# unified ASR + LS-EEND diarizer + pyannote (refine) + Silero VAD. The core copies these into
# FluidAudio's cache dir on first launch, where the sidecars find them and skip the download.
FLUID_REPOS := parakeet-tdt-0.6b-v3 parakeet-unified-en-0.6b ls-eend speaker-diarization silero-vad
FLUID_CACHE := $(HOME)/Library/Application Support/FluidAudio/Models
FLUID_SRC := outputs/models/fluidaudio/Models
FLUID_DST := web/src-tauri/models/fluidaudio/Models

# Stage the ~1.5 GB model, re-copying only when the staged copy is missing or differs from source
# (byte-compare, not just presence — a truncated/outdated staged model would otherwise ship forever).
stage-model: ## Stage the refine model into the Tauri bundle, re-copying if it drifts from source
	@if [ ! -f "$(MODEL_SRC)" ]; then \
		echo "ERROR: refine model $(MODEL_SRC) not found."; \
		echo "Download $(REFINE_MODEL) into outputs/models/ before packaging (see docs/packaging.md)."; \
		exit 1; \
	fi
	@mkdir -p $(dir $(MODEL_DST))
	@if [ ! -f "$(MODEL_DST)" ] || ! cmp -s "$(MODEL_SRC)" "$(MODEL_DST)"; then \
		echo "staging refine model $(REFINE_MODEL)..."; \
		cp "$(MODEL_SRC)" "$(MODEL_DST)"; \
	else \
		echo "refine model already staged (matches source)"; \
	fi

# One-time: copy the needed FluidAudio model repos from the local FluidAudio cache into outputs/ so
# they can be staged reproducibly. Populate the cache first by running the app (or any live sidecar)
# once so FluidAudio downloads them; then re-run this. Keeps packaging inputs under outputs/models/.
fetch-fluid-models: ## Copy the FluidAudio live models from the local cache into outputs/ for packaging
	@for r in $(FLUID_REPOS); do \
		if [ ! -d "$(FLUID_CACHE)/$$r" ]; then \
			echo "ERROR: FluidAudio model '$$r' not in \"$(FLUID_CACHE)\"."; \
			echo "Run Hearsay (or a live sidecar) once to download the models, then re-run 'make fetch-fluid-models'."; \
			exit 1; \
		fi; \
	done
	@mkdir -p "$(FLUID_SRC)"
	@for r in $(FLUID_REPOS); do \
		echo "fetching FluidAudio model $$r..."; \
		rm -rf "$(FLUID_SRC)/$$r"; \
		cp -R "$(FLUID_CACHE)/$$r" "$(FLUID_SRC)/$$r"; \
	done

# Stage the FluidAudio live models into the Tauri bundle (re-copying a repo only when it is missing;
# the model set is pinned to the FluidAudio version, so remove web/src-tauri/models/fluidaudio to
# force a refresh). Runs as part of stage-release so mac-app/dmg always include them.
stage-fluid-models: ## Stage the FluidAudio live models into the Tauri bundle
	@for r in $(FLUID_REPOS); do \
		if [ ! -d "$(FLUID_SRC)/$$r" ]; then \
			echo "ERROR: FluidAudio model '$$r' not in $(FLUID_SRC)."; \
			echo "Run 'make fetch-fluid-models' first (see docs/packaging.md)."; \
			exit 1; \
		fi; \
	done
	@mkdir -p "$(FLUID_DST)"
	@for r in $(FLUID_REPOS); do \
		if [ ! -d "$(FLUID_DST)/$$r" ]; then \
			echo "staging FluidAudio model $$r..."; \
			cp -R "$(FLUID_SRC)/$$r" "$(FLUID_DST)/$$r"; \
		else \
			echo "FluidAudio model $$r already staged"; \
		fi; \
	done

stage-release: stage-model stage-fluid-models ## Build release binaries + web bundle and stage them for the Tauri bundle
	cd web && npm run build
	@for p in $(SIDECARS); do \
		swift build -c release --package-path $(PKG) --product $$p; \
	done
	cargo build --release --manifest-path $(RUST)/Cargo.toml -p hearsay-core --features metal
	@mkdir -p $(STAGE)
	cp $(RUST)/target/release/hearsay-core $(STAGE)/hearsay-core-aarch64-apple-darwin
	@for b in $(SIDECARS); do \
		cp $(PKG)/.build/arm64-apple-macosx/release/$$b $(STAGE)/$$b-aarch64-apple-darwin; \
	done

mac-app: stage-release ## Build the UNSIGNED .app (ad-hoc signed; core + sidecars); needs `cargo install tauri-cli`
	cd web/src-tauri && cargo tauri build --bundles app
	codesign --verify --deep --strict --verbose=2 $(APP)
	@echo "built (unsigned/ad-hoc): $(APP)"

# Depends on mac-app so the .app is signed + verified first; the dmg bundler consumes (and removes)
# the .app, so verification has to happen before this step.
dmg: mac-app ## Build the distributable UNSIGNED .dmg (ad-hoc signed, no notarization)
	# CI=true skips bundle_dmg.sh's Finder AppleScript styling (which needs a GUI session);
	# the drag-to-Applications layout still works, just without custom icon positioning.
	cd web/src-tauri && CI=true cargo tauri build --bundles dmg
	@echo "built (unsigned/ad-hoc): web/src-tauri/target/release/bundle/dmg/ (see docs/packaging.md)"
	@echo "install on another Mac: drag to /Applications, then run"
	@echo "  xattr -dr com.apple.quarantine /Applications/Hearsay.app"
