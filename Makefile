.PHONY: help swift-plist-guard swift-build swift-test rust-build rust-test rust-lint tauri-lint tauri-test rust-fmt test lint fmt codegen codegen-check web-install web-typecheck web-lint web-test web-build web-ci audit licenses version-check version stamp-version set-version ci probes diarize-eval coverage e2e test-all clean-test build package notarize clean serve rust-serve fetch-refine-model fetch-sherpa-models stage-sherpa-models stage-release mac-app dmg

PKG := helper
RUST := rust
FIXTURES := shared/fixtures/frames.jsonl
CONTROL_FIXTURES := shared/fixtures/control.jsonl
# Rust core serve: its own DB (separate from any dev DB) + a stable port.
RUST_DB := sqlite://$(CURDIR)/outputs/db/hearsay-rust.db
RUST_PORT ?= 8799

help: ## Show this help
	@grep -E '^[a-zA-Z_ -]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

# The Swift executables, in one list: the capture helper plus the FluidAudio/ANE sidecars. Both the
# debug build below and the release staging loop over this. `swift build` takes ONE `--product` (a
# repeated flag silently keeps only the last), so each product needs its own invocation.
# SwiftPM does not track the `-sectcreate` plist as a build input, and hashes content so a touch
# won't do: drop the stale binary to force the relink that re-embeds it.
swift-plist-guard:
	@for b in $(PKG)/.build/debug/hearsay-helper $(PKG)/.build/arm64-apple-macosx/release/hearsay-helper; do \
		if [ -f "$$b" ] && [ helper/Info.plist -nt "$$b" ]; then \
			echo "Info.plist changed since $$b was linked; removing it to force a relink"; \
			rm -f "$$b"; \
		fi; \
	done

SWIFT_PRODUCTS := hearsay-helper hearsay-diarize hearsay-live hearsay-me hearsay-models
swift-build: swift-plist-guard ## Build the Swift helper executables (one invocation each; a bare 'swift build' pulls in FluidAudio's CLI target)
	@set -eu; for p in $(SWIFT_PRODUCTS); do \
		swift build --package-path $(PKG) --product $$p; \
	done

swift-test: ## Run the Swift cross-language self-test against the golden fixtures
	swift run --package-path $(PKG) hearsay-helper selftest $(FIXTURES)

rust-build: ## Build the Rust workspace
	cargo build --manifest-path $(RUST)/Cargo.toml

rust-test: ## Run the Rust workspace tests (cargo test)
	cargo test --manifest-path $(RUST)/Cargo.toml

rust-lint: ## Lint Rust (clippy with warnings denied + rustfmt --check)
	cargo clippy --manifest-path $(RUST)/Cargo.toml --all-targets -- -D warnings
	cargo fmt --manifest-path $(RUST)/Cargo.toml --all --check

# tauri-build validates every bundle input at build-script time, and the staged sidecars + web/dist
# exist only after `make stage-release`. Clippy and the tests bundle nothing, so drop those inputs
# from the config the build script reads (TAURI_CONFIG is merged into tauri.conf.json; null deletes).
TAURI_NO_BUNDLE := TAURI_CONFIG='{"bundle":{"externalBin":null,"resources":null}}'

tauri-lint: ## Lint the Tauri shell (the shipping entrypoint; excluded from the rust/ workspace)
	$(TAURI_NO_BUNDLE) cargo clippy --manifest-path web/src-tauri/Cargo.toml --all-targets -- -D warnings
	cargo fmt --manifest-path web/src-tauri/Cargo.toml --check

tauri-test: ## Test the Tauri shell (cargo test on web/src-tauri; excluded from the rust/ workspace)
	$(TAURI_NO_BUNDLE) cargo test --manifest-path web/src-tauri/Cargo.toml

rust-fmt: ## Format Rust (rustfmt)
	cargo fmt --manifest-path $(RUST)/Cargo.toml --all

test: swift-build swift-test rust-test ## Run all tests (Swift + Rust)

lint: rust-lint tauri-lint ## Lint (clippy + rustfmt --check, both the workspace and the Tauri shell)

fmt: rust-fmt ## Format (rustfmt)

codegen: ## Regenerate IPC fixtures + OpenAPI schema + web TS types (Rust is the source of truth)
	cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-ipc --bin gen_fixtures
	cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-core -- --dump-openapi > web/openapi.json.tmp && mv web/openapi.json.tmp web/openapi.json
	cd web && npm run codegen

codegen-check: ## Fail if the committed IPC fixtures / OpenAPI / web types drift from the Rust source
	cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-ipc --bin gen_fixtures
	cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-core -- --dump-openapi > web/openapi.json.tmp && mv web/openapi.json.tmp web/openapi.json
	cd web && npm run codegen
	git diff --exit-code -- $(FIXTURES) $(CONTROL_FIXTURES) web/openapi.json web/src/api/schema.ts

web-install: ## Install pinned web deps (npm ci)
	cd web && npm ci

web-typecheck: ## Type-check the web UI (tsc)
	cd web && npm run typecheck

web-lint: ## Lint the web UI (ESLint: typescript-eslint + react-hooks)
	cd web && npm run lint

web-test: ## Web unit/component tests (vitest, jsdom); also folded into web-ci
	cd web && npm run test

web-build: ## Build the web UI bundle (web/dist)
	cd web && npm run build

web-ci: web-install web-typecheck web-lint web-test web-build ## Web CI gate (install, typecheck, lint, test, build)

audit: ## Dependency CVE scan (cargo-audit over both Rust trees + npm audit)
	cd $(RUST) && cargo audit
	cd web/src-tauri && cargo audit
	# --omit=dev: web dev deps (vite/eslint/vitest/openapi-typescript) are build-time only and never
	# shipped in the Tauri app, so their advisories don't reach users; audit only the production deps.
	cd web && npm audit --omit=dev --audit-level=moderate

licenses: ## Fail the build on copyleft dependency licenses (cargo-deny; policy in rust/deny.toml)
	cargo deny --manifest-path $(RUST)/Cargo.toml check licenses
	cargo deny --manifest-path web/src-tauri/Cargo.toml --config rust/deny.toml check licenses

# rust/Cargo.toml is canonical. The Swift helper is absent: it reads its version from the embedded
# helper/Info.plist at runtime.
VERSION_FILES := $(RUST)/Cargo.toml web/src-tauri/Cargo.toml web/src-tauri/tauri.conf.json \
                 web/package.json helper/Info.plist

# A placeholder in git; the release workflow stamps the real version from the release it is cutting.
version: ## Print the app version as committed (0.0.0 outside a release build)
	@grep -m1 '^version' $(RUST)/Cargo.toml | sed -E 's/.*"(.*)".*/\1/'

# The release build's first step: the committed version is a placeholder, the release workflow
# derives the real one and stamps it here. No codegen, so it runs before `npm ci`.
stamp-version: ## Write the app version into all five files (usage: make stamp-version VERSION=0.2.0)
	@test -n "$(VERSION)" || { echo "usage: make stamp-version VERSION=x.y.z"; exit 1; }
	@echo "$(VERSION)" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$$' || \
		{ echo "ERROR: VERSION must be x.y.z (got '$(VERSION)')"; exit 1; }
	@sed -i '' -E '1,/^version/ s/^version = ".*"/version = "$(VERSION)"/' $(RUST)/Cargo.toml
	@sed -i '' -E '1,/^version/ s/^version = ".*"/version = "$(VERSION)"/' web/src-tauri/Cargo.toml
	@sed -i '' -E '1,/"version"/ s/("version"[[:space:]]*:[[:space:]]*)".*"/\1"$(VERSION)"/' web/src-tauri/tauri.conf.json
	@sed -i '' -E '1,/"version"/ s/("version"[[:space:]]*:[[:space:]]*)".*"/\1"$(VERSION)"/' web/package.json
	@sed -i "" -E '/CFBundleShortVersionString/{n; s|<string>.*</string>|<string>$(VERSION)</string>|;}' helper/Info.plist
	@$(MAKE) --no-print-directory version-check

set-version: stamp-version ## Set the app version everywhere and regenerate codegen (usage: make set-version VERSION=0.2.0)
	# openapi.json embeds the version, so a change without this leaves codegen-check failing.
	@$(MAKE) --no-print-directory codegen

version-check: ## Fail if the app version drifts across the Rust workspace, Tauri shell, package.json, and helper plist
	@set -eu; \
	rust=$$(grep -m1 '^version' $(RUST)/Cargo.toml | sed -E 's/.*"(.*)".*/\1/'); \
	shell=$$(grep -m1 '^version' web/src-tauri/Cargo.toml | sed -E 's/.*"(.*)".*/\1/'); \
	tauri=$$(grep -m1 '"version"' web/src-tauri/tauri.conf.json | sed -E 's/.*: *"(.*)".*/\1/'); \
	pkg=$$(grep -m1 '"version"' web/package.json | sed -E 's/.*: *"(.*)".*/\1/'); \
	plist=$$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" helper/Info.plist); \
	bad=0; \
	for pair in "web/src-tauri/Cargo.toml=$$shell" "tauri.conf.json=$$tauri" "web/package.json=$$pkg" "helper/Info.plist=$$plist"; do \
		if [ "$${pair#*=}" != "$$rust" ]; then echo "ERROR: version drift -- $$pair, expected $$rust"; bad=1; fi; \
	done; \
	if [ $$bad -ne 0 ]; then \
		echo "rust/Cargo.toml is canonical; run 'make set-version VERSION=$$rust' to align the rest."; \
		exit 1; \
	fi; \
	echo "version $$rust consistent across $(words $(VERSION_FILES)) files"

# web-ci comes before codegen-check: that target runs `npm run codegen`, which needs node_modules.
ci: lint test tauri-test web-ci codegen-check version-check audit licenses ## Full CI gate (Rust + Swift + Tauri + web + codegen drift + versions + supply-chain)

# On-demand test suite (docs/testing.md). `make ci` above is the fast deterministic gate; the targets
# below are the model/hardware probes, coverage, and the "run everything" aggregate — run when you want
# on a box that has the models + ANE/GPU. Nothing here is automatic (no timers, no hooks). Windows has
# no `make`, so the same set is mirrored in scripts\test-windows.ps1.
probes: ## Model/hardware tests (the #[ignore]d refine/notes/live probes). Needs the models + ANE/GPU.
	cargo test --manifest-path $(RUST)/Cargo.toml -p hearsay-inference --features metal -- --ignored
	cargo test --manifest-path $(RUST)/Cargo.toml -p hearsay-notes --features metal -- --ignored
	cargo test --manifest-path $(RUST)/Cargo.toml -p hearsay-backends -- --ignored
	cargo test --manifest-path $(RUST)/Cargo.toml -p hearsay-capture -- --ignored

diarize-eval: swift-build ## Diarization accuracy gate: run hearsay-diarize over the local labeled corpus and check speaker-count + DER vs the committed baseline. Self-skips (never fails) when the audio/sidecar are absent, so the same test is safe in `make ci`; here it builds the sidecar and runs it for real with output. Point at a private recording with HEARSAY_DIARIZE_CORPUS + HEARSAY_DIARIZE_BASELINE; re-baseline an intentional change with HEARSAY_UPDATE_DIAR_BASELINE=1.
	cargo test --manifest-path $(RUST)/Cargo.toml -p hearsay-inference --test diarization_accuracy diarization_accuracy_gate -- --nocapture

coverage: ## Coverage report (cargo-llvm-cov + vitest v8) into outputs/coverage/ (report-only)
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { echo "cargo-llvm-cov not installed: run 'cargo install cargo-llvm-cov'"; exit 1; }
	@mkdir -p outputs/coverage/rust
	cargo llvm-cov clean --workspace --manifest-path $(RUST)/Cargo.toml
	cargo llvm-cov --no-report --workspace --manifest-path $(RUST)/Cargo.toml
	cargo llvm-cov report --lcov --output-path outputs/coverage/rust/lcov.info --manifest-path $(RUST)/Cargo.toml
	cargo llvm-cov report --html --output-dir outputs/coverage/rust --manifest-path $(RUST)/Cargo.toml
	cd web && npm run coverage

e2e: ## Browser end-to-end (Playwright) vs the scripted core + vite. One-time: cd web && npm install && npx playwright install chromium
	@command -v npx >/dev/null 2>&1 || { echo "npx not found: install Node (https://nodejs.org)"; exit 1; }
	@test -d web/node_modules/@playwright/test || { echo "playwright not installed: run 'cd web && npm install' (then 'npx playwright install chromium')"; exit 1; }
	@mkdir -p outputs/e2e
	# Build the core up front so Playwright's webServer starts it fast (no cold cargo build under the
	# start timeout). The scripted engine is platform-neutral, so default features suffice on macOS.
	cargo build --manifest-path $(RUST)/Cargo.toml -p hearsay-core
	# An archived (compressed-only) meeting recording for the playback spec, built by the real encoder.
	cargo run -q --manifest-path $(RUST)/Cargo.toml -p hearsay-audio --example fixture -- outputs/e2e/fixture
	cd web && npx playwright test

test-all: ci probes e2e ## Run everything: the deterministic gate + the model/hardware probes + the browser E2E

clean-test: ## Remove the coverage + e2e report dirs (test data auto-cleans via tempdirs)
	rm -rf outputs/coverage outputs/e2e

build notarize: ## Notarized distribution (needs a paid Apple Developer account)
	@echo "$@: needs an Apple Developer ID + notarization; use 'make dmg' for the unsigned build"

package: dmg ## Build the distributable DMG (alias for 'dmg')

clean: clean-test ## Remove build artifacts (Swift, Rust, web bundle + deps, Tauri target, staged binaries/models)
	rm -rf $(PKG)/.build $(RUST)/target web/dist web/node_modules web/src-tauri/target $(STAGE) $(MODELS_STAGE)

serve rust-serve: ## Serve the Rust core (SYNTHETIC=1 for no-permission plumbing; needs swift-build + web-build for a live run)
	@mkdir -p outputs/db
	# Build the notes sidecar so the core (which spawns it as a `hearsay-notes` sibling) resolves it in
	# dev. Separate binary + separate build so llama.cpp never co-links with whisper (a ggml collision
	# that slows the refine ~5x); the core is built WITHOUT a notes feature.
	cargo build --manifest-path $(RUST)/Cargo.toml -p hearsay-notes --features metal
	HEARSAY_SERVER_PORT=$(RUST_PORT) DATABASE_URL="$(RUST_DB)" \
		cargo run --manifest-path $(RUST)/Cargo.toml -p hearsay-core --features metal,aec,api-console $(if $(SYNTHETIC),-- --synthetic)

# Distribution staging: build RELEASE binaries + web bundle, then copy them where Tauri's
# `externalBin` expects them (`<name>-<target-triple>`). Shared by `mac-app` and `dmg`.
STAGE := web/src-tauri/binaries
SIDECARS := $(SWIFT_PRODUCTS)
APP := web/src-tauri/target/release/bundle/macos/Hearsay.app
# Where the Windows staging targets put their models. The macOS bundle carries none — the app
# downloads them on first run (docs/packaging.md).
MODELS_STAGE := web/src-tauri/models
# The refine model a dev run loads (the default `HEARSAY_REFINE_MODEL` path).
REFINE_MODEL := ggml-large-v3-turbo.bin
MODEL_SRC := outputs/models/$(REFINE_MODEL)
WHISPER_REPO := https://huggingface.co/ggerganov/whisper.cpp/resolve/main

fetch-refine-model: ## Download the whisper refine model into outputs/models/ (for a dev run; the app downloads its own)
	@if [ -f "$(MODEL_SRC)" ]; then echo "refine model already present"; else \
		mkdir -p outputs/models; \
		echo "fetching $(REFINE_MODEL) (about 1.5 GB)..."; \
		curl -fL --retry 3 -o "$(MODEL_SRC).part" "$(WHISPER_REPO)/$(REFINE_MODEL)"; \
		mv "$(MODEL_SRC).part" "$(MODEL_SRC)"; \
	fi

# Sherpa live/diarize models for the Windows backend: the streaming
# zipformer + pyannote segmentation + TitaNet-small embedder, all from the sherpa-onnx model zoo
# ("recongition" is the real upstream release-tag spelling). Fetch works from any host with
# curl+tar; scripts/build-windows.ps1 does the same on the Windows machine.
SHERPA_RELEASE := https://github.com/k2-fsa/sherpa-onnx/releases/download
SHERPA_STREAMING := sherpa-onnx-streaming-zipformer-en-2023-06-21
SHERPA_SEGMENTATION := sherpa-onnx-pyannote-segmentation-3-0
# Restores case + punctuation on the streaming zipformer's bare uppercase output.
SHERPA_PUNCT := sherpa-onnx-online-punct-en-2024-08-06
SHERPA_EMBEDDING := nemo_en_titanet_small.onnx
SHERPA_SRC := outputs/models/sherpa
SHERPA_DST := web/src-tauri/models/sherpa

fetch-sherpa-models: ## Download the sherpa live/diarize models (Windows backend) into outputs/
	@mkdir -p "$(SHERPA_SRC)"
	@set -euo pipefail; for a in $(SHERPA_STREAMING) $(SHERPA_SEGMENTATION) $(SHERPA_PUNCT); do \
		if [ -d "$(SHERPA_SRC)/$$a" ]; then \
			echo "sherpa model $$a already fetched"; \
		else \
			echo "fetching sherpa model $$a..."; \
			tag=asr-models; \
			if [ "$$a" = "$(SHERPA_SEGMENTATION)" ]; then tag=speaker-segmentation-models; fi; \
			if [ "$$a" = "$(SHERPA_PUNCT)" ]; then tag=punctuation-models; fi; \
			curl -fL "$(SHERPA_RELEASE)/$$tag/$$a.tar.bz2" | tar xjf - -C "$(SHERPA_SRC)"; \
		fi; \
	done
	@if [ -f "$(SHERPA_SRC)/$(SHERPA_EMBEDDING)" ]; then \
		echo "sherpa model $(SHERPA_EMBEDDING) already fetched"; \
	else \
		echo "fetching sherpa model $(SHERPA_EMBEDDING)..."; \
		curl -fL -o "$(SHERPA_SRC)/$(SHERPA_EMBEDDING)" \
			"$(SHERPA_RELEASE)/speaker-recongition-models/$(SHERPA_EMBEDDING)"; \
	fi

stage-sherpa-models: ## Stage the sherpa models into the Tauri bundle (Windows packaging)
	@for m in $(SHERPA_STREAMING) $(SHERPA_SEGMENTATION) $(SHERPA_PUNCT) $(SHERPA_EMBEDDING); do \
		if [ ! -e "$(SHERPA_SRC)/$$m" ]; then \
			echo "ERROR: sherpa model '$$m' not in $(SHERPA_SRC)."; \
			echo "Run 'make fetch-sherpa-models' first."; \
			exit 1; \
		fi; \
	done
	@mkdir -p "$(SHERPA_DST)"
	@set -euo pipefail; for m in $(SHERPA_STREAMING) $(SHERPA_SEGMENTATION) $(SHERPA_PUNCT) $(SHERPA_EMBEDDING); do \
		if [ -e "$(SHERPA_DST)/$$m" ]; then \
			echo "sherpa model $$m already staged"; \
		elif [ "$$m" = "$(SHERPA_STREAMING)" ]; then \
			echo "staging sherpa model $$m (int8 only)..."; \
			mkdir -p "$(SHERPA_DST)/$$m"; \
			cp "$(SHERPA_SRC)/$$m"/*.int8.onnx "$(SHERPA_SRC)/$$m"/tokens.txt "$(SHERPA_DST)/$$m/"; \
		else \
			echo "staging sherpa model $$m..."; \
			cp -R "$(SHERPA_SRC)/$$m" "$(SHERPA_DST)/$$m"; \
		fi; \
	done

stage-release: version-check swift-plist-guard web-install ## Build release binaries + web bundle and stage them for the Tauri bundle
	@test "$$(uname -m)" = "arm64" || { echo "stage-release: Apple-Silicon (arm64) only (got $$(uname -m)); the bundle is Apple-Silicon only"; exit 1; }
	cd web && npm run build
	@for p in $(SIDECARS); do \
		swift build -c release --package-path $(PKG) --product $$p; \
	done
	# The core is built WITHOUT notes; the notes LLM ships as its own `hearsay-notes` sidecar so
	# llama.cpp never co-links with whisper (a ggml collision that slows the refine ~5x). Both get the
	# same `metal` accel.
	cargo build --release --manifest-path $(RUST)/Cargo.toml -p hearsay-core --features metal,aec
	cargo build --release --manifest-path $(RUST)/Cargo.toml -p hearsay-notes --features metal
	@mkdir -p $(STAGE)
	cp $(RUST)/target/release/hearsay-core $(STAGE)/hearsay-core-aarch64-apple-darwin
	cp $(RUST)/target/release/hearsay-notes $(STAGE)/hearsay-notes-aarch64-apple-darwin
	@set -euo pipefail; for b in $(SIDECARS); do \
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
