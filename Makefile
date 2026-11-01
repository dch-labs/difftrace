.PHONY: build release install check test clippy fmt docs ci lint eval-fmt eval-clippy eval-test eval-golden eval-mutations eval-baseline eval-run-v1 eval-run-current eval-parity

ci: fmt check clippy test docs eval-fmt eval-clippy eval-test

build:
	cargo build --all-features

release:
	cargo build --release --all-features

install: release
	@mkdir -p $(HOME)/.local/bin
	@rm -f $(HOME)/.local/bin/difftrace
	@cp target/release/difftrace $(HOME)/.local/bin/difftrace
	@chmod +x $(HOME)/.local/bin/difftrace
	@echo "✅ Installed to $(HOME)/.local/bin/difftrace"

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
	cargo run -q --manifest-path eval/Cargo.toml --bin eval-golden -- --transcripts $(HOME)/.difftrace/reviews --repo difftrace=. --repo dch=../dch --repo loopctl=../loopctl --out eval/golden/index.json

eval-mutations:
	cargo run -q --manifest-path eval/Cargo.toml --bin eval-mutate -- --repo dch=../dch --repo loopctl=../loopctl

eval-baseline:
	cargo run -q --manifest-path eval/Cargo.toml --bin eval-run -- --golden

eval-run-v1:
	cargo run -q --manifest-path eval/Cargo.toml --bin eval-run -- --mutations --repo dch=../dch --repo loopctl=../loopctl

eval-run-current:
	cargo run -q --manifest-path eval/Cargo.toml --bin eval-run -- --mutations --runner current --binary target/release/difftrace --repo dch=../dch --repo loopctl=../loopctl

eval-parity:
	cargo run -q --manifest-path eval/Cargo.toml --bin eval-run -- --parity --v1 eval/reports/v1-metrics.json --current eval/reports/current-metrics.json
