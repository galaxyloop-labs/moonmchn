CARGO ?= cargo

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

.PHONY: fmt
fmt: ## Format all code
	$(CARGO) fmt --all

.PHONY: fmt-check
fmt-check: ## Fail if any file is not formatted
	$(CARGO) fmt --all --check

.PHONY: check
check: ## Type-check the workspace, then the core library independently
	$(CARGO) check --workspace --all-targets --all-features
	$(CARGO) check -p moonmchn --all-targets --no-default-features

.PHONY: lint
lint: ## Run clippy on every target; warnings are errors
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

.PHONY: test
test: ## Run unit, integration and doc tests
	$(CARGO) test --workspace --all-features

.PHONY: test-unit
test-unit: ## Run unit tests only (libraries and binary)
	$(CARGO) test --workspace --all-features --lib --bins

.PHONY: test-integration
test-integration: ## Run integration tests only (crates/*/tests/)
	$(CARGO) test --workspace --all-features --test '*'

.PHONY: doc
doc: ## Build API docs; broken links and missing docs are errors
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps --all-features

.PHONY: build
build: ## Debug build of all workspace crates
	$(CARGO) build --workspace

.PHONY: release
release: ## Optimised build of all workspace crates
	$(CARGO) build --workspace --release

.PHONY: audit
audit: ## Check dependencies against the RustSec advisory database
	$(CARGO) audit

.PHONY: deny
deny: ## Check licenses, bans, advisories and sources (deny.toml)
	$(CARGO) deny check

.PHONY: outdated
outdated: ## List dependencies with newer versions available
	$(CARGO) outdated

.PHONY: ci
ci: fmt-check check lint test doc ## Everything CI runs

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
