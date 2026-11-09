CARGO ?= cargo
NIX ?= nix
CLIFF ?= git-cliff
ACTIONLINT ?= actionlint
SHELLCHECK ?= shellcheck
ZIZMOR ?= zizmor

# The release version is [workspace.package] version in Cargo.toml. Cargo
# cannot read a version from another file, so a VERSION file would be a second
# copy. Recursive assignment: awk runs only for targets that use it.
VERSION = $(shell awk -F'"' '/^\[/ { in_section = ($$0 ~ /^\[workspace\.package\]/) } in_section && /^version[ \t]*=/ { print $$2; exit }' Cargo.toml)

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

.PHONY: version
version: ## Print the release version from Cargo.toml
	@echo '$(VERSION)'

.PHONY: fmt
fmt: ## Format all code, Rust and Nix
	$(CARGO) fmt --all
	$(NIX) fmt

.PHONY: fmt-check
fmt-check: ## Fail if any Rust file is not formatted
	$(CARGO) fmt --all --check

.PHONY: nix-fmt-check
nix-fmt-check: ## Fail if any Nix file is not formatted
	$(NIX) fmt -- --ci

.PHONY: workflows-check
workflows-check: ## Lint the GitHub Actions workflows and their scripts; audit them and Dependabot's config
	$(ACTIONLINT) -shellcheck=$(SHELLCHECK)
	$(SHELLCHECK) .github/scripts/*.sh
	$(ZIZMOR) --offline .

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

.PHONY: changelog
changelog: ## Regenerate CHANGELOG.md from git history (git-cliff, cliff.toml)
	@version='$(VERSION)'; \
	if ! printf '%s' "$$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$$'; then \
		echo 'changelog: no MAJOR.MINOR.PATCH version under [workspace.package] in Cargo.toml' >&2; \
		exit 1; \
	fi; \
	export GITHUB_TOKEN="$${GITHUB_TOKEN:-$$(gh auth token 2>/dev/null)}"; \
	if git rev-parse -q --verify "refs/tags/v$$version" >/dev/null; then \
		$(CLIFF) --output CHANGELOG.md; \
	else \
		$(CLIFF) --tag "v$$version" --output CHANGELOG.md; \
	fi

# A default macOS terminal allows 256 open files per process. Under the higher
# limit of CI runners and other shells, a test that runs out of descriptors on
# a Mac would pass, so the whole run gets that soft limit.
.PHONY: ci
ci: ## Everything CI runs, at the 256 open files of a default macOS terminal
	ulimit -S -n 256 && $(MAKE) --no-print-directory fmt-check nix-fmt-check workflows-check check lint test doc

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
