.PHONY: check test clippy fmt docs ci lint e2e

ci: fmt check clippy test docs

check:
	cargo check --all-features

test:
	cargo test
	cargo test --all-features
	cargo test --doc --all-features

clippy:
	cargo clippy --all-targets --all-features -- -D warnings

fmt:
	cargo fmt -- --check

lint:
	cargo fmt

docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

e2e:
	@test -n "$(GITHUB_TOKEN)" && test -n "$(DIFFTRACE_TEST_REPO)" && test -n "$(DIFFTRACE_TEST_PR)" || { echo "ERROR: set GITHUB_TOKEN, DIFFTRACE_TEST_REPO (owner/repo), DIFFTRACE_TEST_PR"; exit 1; }
	GITHUB_TOKEN=$(GITHUB_TOKEN) cargo run --all-features -- review --repo $(DIFFTRACE_TEST_REPO) --pr $(DIFFTRACE_TEST_PR) --dry-run
