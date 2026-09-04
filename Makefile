.PHONY: check test clippy fmt docs ci lint e2e

ci: fmt check clippy test docs

check:
	cargo check --all-features

# the log-capture tests install thread-local tracing subscribers,
# which is only reliable when the test binaries run serially
test:
	cargo test -- --test-threads=1
	cargo test --all-features -- --test-threads=1
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
	DIFFTRACE_E2E=1 GITHUB_TOKEN=$(GITHUB_TOKEN) DIFFTRACE_TEST_REPO=$(DIFFTRACE_TEST_REPO) DIFFTRACE_TEST_PR=$(DIFFTRACE_TEST_PR) cargo test --test github_live
	GITHUB_TOKEN=$(GITHUB_TOKEN) cargo run --all-features -- review --repo $(DIFFTRACE_TEST_REPO) --pr $(DIFFTRACE_TEST_PR) --dry-run
