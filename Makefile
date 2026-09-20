.PHONY: check test clippy fmt docs ci lint e2e eval-fmt eval-clippy eval-test eval-golden

ci: fmt check clippy test docs eval-fmt eval-clippy eval-test

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

eval-fmt:
	cargo fmt --manifest-path eval/Cargo.toml -- --check

eval-clippy:
	cargo clippy --manifest-path eval/Cargo.toml --all-targets -- -D warnings

eval-test:
	cargo test --manifest-path eval/Cargo.toml

eval-golden:
	cargo run --manifest-path eval/Cargo.toml -- --transcripts $(HOME)/.difftrace/reviews --repo difftrace=. --repo dch=../dch --repo loopctl=../loopctl --out eval/golden/index.json

e2e:
	@test -n "$(GITHUB_TOKEN)" && test -n "$(DIFFTRACE_TEST_REPO)" && test -n "$(DIFFTRACE_TEST_PR)" || { echo "ERROR: set GITHUB_TOKEN, DIFFTRACE_TEST_REPO (owner/repo), DIFFTRACE_TEST_PR"; exit 1; }
	DIFFTRACE_E2E=1 GITHUB_TOKEN=$(GITHUB_TOKEN) DIFFTRACE_TEST_REPO=$(DIFFTRACE_TEST_REPO) DIFFTRACE_TEST_PR=$(DIFFTRACE_TEST_PR) cargo test --test github_live
	GITHUB_TOKEN=$(GITHUB_TOKEN) cargo run --all-features -- review --repo $(DIFFTRACE_TEST_REPO) --pr $(DIFFTRACE_TEST_PR) --dry-run
