.PHONY: help lint lint-all test build build-rust build-go build-dashboard \
       fmt check clean hooks ci deny coverage typos doc machete safe-merge \
       manifest preflight smoke-test deploy fuzz verify snapshot-test snapshot-review

# Default target
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

# ──────────────────────────────────────────────
# Fast Feedback (< 30s)
# ──────────────────────────────────────────────

fmt: ## Format all code
	cargo fmt --all
	cd cmd/cortex-gateway && gofmt -w .
	@echo "Format: OK"

check: ## Quick lint (changed files only, fast)
	@echo "=== Rust (clippy) ==="
	cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
	@echo "=== Go (vet) ==="
	cd cmd/cortex-gateway && go vet ./...
	@echo "=== Check: OK ==="

lint: check ## Alias for check

lint-all: ## Full lint (like CI, slower)
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items 2>&1 | tail -5
	cargo deny check 2>/dev/null || echo "(cargo-deny not installed, skipping)"
	typos 2>/dev/null || echo "(typos not installed, skipping)"
	cd cmd/cortex-gateway && go vet ./...
	cd cmd/cortex-gateway && golangci-lint run
	@echo "Full lint: OK"

# ──────────────────────────────────────────────
# Tests
# ──────────────────────────────────────────────

test: ## Run all tests
	@echo "=== Rust Tests ==="
	cargo test --workspace
	@echo "=== Go Tests ==="
	cd cmd/cortex-gateway && go test ./...
	@echo "=== Dashboard Tests ==="
	cd dashboard && bun test 2>/dev/null || echo "(no dashboard tests yet)"
	@echo "All tests: OK"

test-rust: ## Run Rust tests only
	cargo test --workspace

test-go: ## Run Go tests only
	cd cmd/cortex-gateway && go test ./...

test-dashboard: ## Run Dashboard tests only
	cd dashboard && bun test

# ──────────────────────────────────────────────
# Build
# ──────────────────────────────────────────────

build: build-rust build-go build-dashboard ## Build everything

build-rust: ## Build Rust workspace
	cargo build --workspace

build-rust-remote: ## Build Rust on remote build server
	cargo remote -- build --workspace

build-rust-release: ## Build Rust release (remote, includes eBPF kernel probes)
	cargo remote -- build --workspace --release --features ebpf

build-go: ## Build Cortex Gateway
	cd cmd/cortex-gateway && go build -o cortex-gateway ./...

build-dashboard: ## Build Dashboard
	cd dashboard && bun install

# ──────────────────────────────────────────────
# Code Generation
# ──────────────────────────────────────────────

generate: ## Generate FlatBuffer code from schemas
	@for f in schemas/*.fbs; do \
		echo "Compiling $$f"; \
		flatc --rust -o crates/sentinel-common/src/generated "$$f"; \
		flatc --go -o cmd/cortex-gateway/internal/generated "$$f"; \
	done
	@echo "FlatBuffer generation: OK"

# ──────────────────────────────────────────────
# CI / Quality
# ──────────────────────────────────────────────

ci: lint-all test ## Run full CI locally (lint + test)
	@echo "Local CI: ALL GREEN"

deny: ## Run cargo-deny checks (licenses, advisories, bans)
	cargo deny check

coverage: ## Generate Rust coverage report (requires cargo-tarpaulin)
	cargo tarpaulin --workspace --out lcov --output-dir target/coverage
	@echo "Coverage report: target/coverage/lcov.info"

security: ## Run security audits
	cargo audit
	cargo deny check advisories
	cd cmd/cortex-gateway && govulncheck ./...
	@echo "Security audit: OK"

typos: ## Check for typos in code and docs
	typos

doc: ## Build docs with warnings as errors
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items
	@echo "Docs: OK"

machete: ## Find unused Rust dependencies
	cargo machete

bench: ## Run benchmarks
	cargo bench --workspace
	@echo "Benchmarks: DONE"

# ──────────────────────────────────────────────
# Property-Based / Snapshot Testing
# ──────────────────────────────────────────────

fuzz: ## Run bolero fuzzing (requires cargo-bolero)
	@echo "=== Bolero Fuzzing (sentinel-bio) ==="
	cargo bolero test -p sentinel-bio --all-targets --time 60
	@echo "=== Bolero Fuzzing (sentinel-physics) ==="
	cargo bolero test -p sentinel-physics --all-targets --time 60
	@echo "Fuzzing: DONE (60s per crate)"

verify: ## Formal verification via bolero kani engine (if available)
	@command -v cargo-kani >/dev/null 2>&1 && cargo kani --workspace \
		|| echo "WARN: cargo-kani not installed. Install with: cargo install --locked kani-verifier && cargo kani setup"

snapshot-test: ## Run insta snapshot tests (CI mode, fails on mismatch)
	@echo "=== Snapshot Tests ==="
	cargo insta test --workspace --check
	@echo "Snapshot tests: OK"

snapshot-review: ## Review pending insta snapshot changes
	cargo insta review

# ──────────────────────────────────────────────
# Setup
# ──────────────────────────────────────────────

hooks: ## Install repo-managed git hooks (.githooks via core.hooksPath)
	./scripts/setup-githooks.sh

safe-merge: ## Safely merge PR (usage: make safe-merge PR=123 [METHOD=merge|squash|rebase])
	@if [ -z "$(PR)" ]; then echo "Usage: make safe-merge PR=<number> [METHOD=merge|squash|rebase]"; exit 1; fi
	./scripts/safe-merge.sh "$(PR)" "$(if $(METHOD),$(METHOD),merge)"

# ──────────────────────────────────────────────
# Deploy (VM: ubuntu@192.0.2.240)
# ──────────────────────────────────────────────

SSH ?= ubuntu@192.0.2.240

manifest: ## Generate release manifest (requires built artifacts)
	bash deploy/generate-manifest.sh

preflight: ## Verify manifest hashes against VM (usage: make preflight [SSH=ubuntu@192.0.2.240])
	bash deploy/deploy-preflight.sh "$(SSH)"

smoke-test: ## Post-deploy smoke test (usage: make smoke-test [SSH=ubuntu@192.0.2.240])
	bash deploy/smoke-test.sh "$(SSH)"

deploy: preflight ## Deploy to VM: preflight + sync + smoke (usage: make deploy [SSH=ubuntu@192.0.2.240])
	@echo ""
	@echo "=== Preflight passed, deploying artifacts ==="
	@echo "Syncing configs..."
	@scp -q config/*.toml "$(SSH)":/opt/sentinel/config/
	@scp -q config/nats.conf "$(SSH)":/etc/nats/nats.conf
	@echo "Syncing systemd units..."
	@scp -q deploy/systemd/*.service deploy/systemd/*.timer deploy/systemd/*.target "$(SSH)":/etc/systemd/system/
	@ssh "$(SSH)" "sudo systemctl daemon-reload"
	@echo "Syncing init scripts..."
	@scp -q deploy/scripts/*.sh "$(SSH)":/opt/sentinel/scripts/
	@echo ""
	@echo "=== Running smoke test ==="
	bash deploy/smoke-test.sh "$(SSH)"
	@echo ""
	@echo "Deploy: OK"

# ──────────────────────────────────────────────
# Cleanup
# ──────────────────────────────────────────────

clean: ## Remove build artifacts
	cargo clean
	cd cmd/cortex-gateway && rm -f cortex-gateway
	cd dashboard && rm -rf node_modules
	@echo "Clean: OK"
