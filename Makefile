# No implicit rules, no built-in suffixes: every target below is a command,
# not a file.
.SUFFIXES:

.PHONY: help build test lint fmt check release

# Bump level for `make release`: patch, minor, major, or an exact version.
LEVEL ?= patch

help:
	@echo 'Targets:'
	@echo '  build              cargo build --release'
	@echo '  test               cargo test --workspace'
	@echo '  lint               cargo clippy, denying warnings'
	@echo '  fmt                cargo fmt --all'
	@echo '  check              what CI runs as the PR gate'
	@echo '  release            bump the version and cut a release commit + tag'
	@echo
	@echo 'Variables:'
	@echo '  LEVEL=patch|minor|major|X.Y.Z   bump for `make release` (default: patch)'

build:
	cargo build --release --locked

test:
	cargo test --workspace --all-features

lint:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

fmt:
	cargo fmt --all

check: lint test
	cargo fmt --all --check

# Bumps both crates, commits, and tags locally. Pushing the tag is left to the
# operator: that push is what fires the release workflow.
release: check
	@command -v cargo-release >/dev/null || { \
		echo 'cargo-release is not installed: cargo install cargo-release'; \
		exit 1; \
	}
	cargo release $(LEVEL) --execute --no-confirm
	@tag=$$(git describe --tags --abbrev=0); \
		echo; \
		echo "Tagged $$tag. Push it to cut the GitHub release:"; \
		echo; \
		echo "    git push origin main $$tag"; \
		echo
