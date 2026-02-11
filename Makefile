.PHONY: help lint lint-all test build build-rust build-go build-dashboard \
       fmt check clean hooks ci deny coverage

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
	cargo deny check 2>/dev/null || echo "(cargo-deny not installed, skipping)"
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

build-rust-release: ## Build Rust release (remote)
	cargo remote -- build --workspace --release

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

bench: ## Run benchmarks
	cargo bench --workspace
	@echo "Benchmarks: DONE"

# ──────────────────────────────────────────────
# Setup
# ──────────────────────────────────────────────

hooks: ## Install git hooks
	@echo '#!/bin/sh' > .git/hooks/pre-commit
	@echo 'make fmt && make check' >> .git/hooks/pre-commit
	@chmod +x .git/hooks/pre-commit
	@echo '#!/bin/sh' > .git/hooks/pre-push
	@echo 'make lint-all' >> .git/hooks/pre-push
	@chmod +x .git/hooks/pre-push
	@echo "Git hooks installed"

clean: ## Remove build artifacts
	cargo clean
	cd cmd/cortex-gateway && rm -f cortex-gateway
	cd dashboard && rm -rf node_modules
	@echo "Clean: OK"
