CARGO ?= cargo
RUSTFMT ?= rustfmt
NIX ?= nix
CLIFF ?= git-cliff
ACTIONLINT ?= actionlint
SHELLCHECK ?= shellcheck
ZIZMOR ?= zizmor

# The release version is [workspace.package] version in Cargo.toml. Cargo
# cannot read a version from another file, so a VERSION file would be a second
# copy. Recursive assignment: awk runs only for targets that use it.
VERSION = $(shell awk -F'"' '/^\[/ { in_section = ($$0 ~ /^\[workspace\.package\]/) } in_section && /^version[ \t]*=/ { print $$2; exit }' Cargo.toml)

# A release version, MAJOR.MINOR.PATCH, as an extended regular expression for `grep -E`.
SEMVER = ^[0-9]+\.[0-9]+\.[0-9]+$$

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

.PHONY: version
version: ## Print the release version from Cargo.toml
	@echo '$(VERSION)'

# Cargo needs the version written again on each path dependency to publish, and cannot read it
# from [workspace.package], so one command writes every copy. fuzz/ pins the crates' versions in
# its own lockfile; `cargo metadata` rewrites them there without updating anything else.
.PHONY: bump
bump: ## Set every crate's version: make bump TO=MAJOR.MINOR.PATCH
	@if ! printf '%s' '$(TO)' | grep -Eq '$(SEMVER)'; then \
		echo 'bump: usage: make bump TO=MAJOR.MINOR.PATCH' >&2; \
		exit 1; \
	fi
	$(CARGO) set-version --workspace '$(TO)'
	$(CARGO) metadata --manifest-path fuzz/Cargo.toml --format-version 1 >/dev/null

.PHONY: fmt
fmt: ## Format all code, Rust and Nix
	$(CARGO) fmt --all
	$(RUSTFMT) --edition 2024 $(FUZZ_SOURCES)
	$(NIX) fmt

.PHONY: fmt-check
fmt-check: ## Fail if any Rust file is not formatted
	$(CARGO) fmt --all --check
	$(RUSTFMT) --edition 2024 --check $(FUZZ_SOURCES)

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

.PHONY: audit
audit: ## Check both lockfiles, the workspace's and fuzz/'s, against the RustSec advisory database
	$(CARGO) audit
	$(CARGO) audit --file fuzz/Cargo.lock

.PHONY: deny
deny: ## Check licenses, bans, advisories and sources (deny.toml)
	$(CARGO) deny check

.PHONY: outdated
outdated: ## List dependencies with newer versions available
	$(CARGO) outdated

.PHONY: changelog
changelog: ## Regenerate CHANGELOG.md from git history (git-cliff, cliff.toml)
	@version='$(VERSION)'; \
	if ! printf '%s' "$$version" | grep -Eq '$(SEMVER)'; then \
		echo 'changelog: no MAJOR.MINOR.PATCH version under [workspace.package] in Cargo.toml' >&2; \
		exit 1; \
	fi; \
	export GITHUB_TOKEN="$${GITHUB_TOKEN:-$$(gh auth token 2>/dev/null)}"; \
	if git rev-parse -q --verify "refs/tags/v$$version" >/dev/null; then \
		$(CLIFF) --output CHANGELOG.md; \
	else \
		$(CLIFF) --tag "v$$version" --output CHANGELOG.md; \
	fi

# Pushing the tag starts .github/workflows/release.yml, which publishes to crates.io: an upload
# cannot be taken back, so every check runs before anything changes, and the push waits for a yes.
# `--atomic`: origin gets main and the tag together, or neither.
.PHONY: release
release: ## Release TO=MAJOR.MINOR.PATCH: check, bump, changelog, commit, signed tag, push
	@if ! printf '%s' '$(TO)' | grep -Eq '$(SEMVER)'; then \
		echo 'release: usage: make release TO=MAJOR.MINOR.PATCH' >&2; \
		exit 1; \
	fi
	@if [ -n "$$(git status --porcelain)" ]; then \
		echo 'release: the working tree has changes; commit or stash them' >&2; \
		exit 1; \
	fi
	@if [ "$$(git branch --show-current)" != main ]; then \
		echo 'release: not on main' >&2; \
		exit 1; \
	fi
	git fetch --quiet origin main
	@if [ "$$(git rev-parse HEAD)" != "$$(git rev-parse origin/main)" ]; then \
		echo 'release: main differs from origin/main; pull or push first' >&2; \
		exit 1; \
	fi
	@if git rev-parse -q --verify 'refs/tags/v$(TO)' >/dev/null || \
		git ls-remote --exit-code --tags origin 'refs/tags/v$(TO)' >/dev/null; then \
		echo 'release: tag v$(TO) already exists' >&2; \
		exit 1; \
	fi
	$(MAKE) --no-print-directory ci
	$(MAKE) --no-print-directory bump TO='$(TO)'
	$(MAKE) --no-print-directory changelog
	$${EDITOR:-vi} CHANGELOG.md
	git commit --all --message 'chore: release v$(TO)'
	git tag --sign --message 'v$(TO)' 'v$(TO)'
	@git --no-pager show --stat HEAD
	@printf 'Push main and v$(TO) to origin, starting the release? [y/N] '; \
	read -r answer; \
	case "$$answer" in \
		y|Y) git push --atomic origin main 'v$(TO)' ;; \
		*) echo 'release: not pushed; to undo: git tag -d v$(TO) && git reset --keep HEAD~1' >&2; exit 1 ;; \
	esac

# The fuzz targets under fuzz/, a workspace of its own, and the seeds each starts from. `cargo fuzz`
# keeps the inputs that reach new code in fuzz/corpus/<target>, and a failing one in
# fuzz/artifacts/<target>. `cargo fmt --all` covers only the main workspace, so `fmt` names these.
FUZZ_TARGETS := preamble claimed_name
FUZZ_SOURCES = $(wildcard fuzz/fuzz_targets/*.rs)
FUZZ_SEEDS_preamble := fuzz/seeds/preamble
FUZZ_SEEDS_claimed_name := crates/moonmchn/src/daemon/decider/testdata
FUZZ_SECONDS ?= 60
# The longest request a gateway writes: an IPv6 preamble's 93 bytes, then 8 KiB of opening bytes.
FUZZ_MAX_LEN := 8285
# Seconds one input may run before it counts as a hang.
FUZZ_INPUT_TIMEOUT := 10
FUZZ_RUNS := $(addprefix fuzz-,$(FUZZ_TARGETS))

# `--sanitizer none`: the crates forbid `unsafe`, so AddressSanitizer would watch only their
# dependencies, and it needs nightly Rust where the flake pins stable. Panics, hangs, runaway
# memory and failed debug assertions still fail a run.
.PHONY: fuzz $(FUZZ_RUNS)
fuzz: $(FUZZ_RUNS) ## Fuzz each target for FUZZ_SECONDS seconds (default 60), from its seeds and corpus

$(FUZZ_RUNS): fuzz-%:
	mkdir -p fuzz/corpus/$*
	$(CARGO) fuzz run --sanitizer none $* fuzz/corpus/$* $(FUZZ_SEEDS_$*) -- \
		-max_total_time=$(FUZZ_SECONDS) -max_len=$(FUZZ_MAX_LEN) -timeout=$(FUZZ_INPUT_TIMEOUT)

# A default macOS terminal allows 256 open files per process. Under the higher
# limit of CI runners and other shells, a test that runs out of descriptors on
# a Mac would pass, so the whole run gets that soft limit.
.PHONY: ci
ci: ## Everything CI runs, at the 256 open files of a default macOS terminal
	ulimit -S -n 256 && $(MAKE) --no-print-directory fmt-check nix-fmt-check workflows-check check lint test doc

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
