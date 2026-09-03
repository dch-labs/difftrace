# difftrace

An AI pull-request reviewer for GitHub, built on
[loopctl](https://github.com/dch-labs/loopctl).

difftrace parses a pull request's unified diff into an index that decides
which lines may anchor a review comment: a citation outside the changed
hunks is rejected, never guessed into place. It then drives an LLM agent
loop over the changed files in batches, records findings through a typed
schema, grounds every finding against the diff, summarizes what survived,
and posts one atomic review — or renders it locally with `--dry-run`.

## Requirements

- Rust 1.98 or newer
- A GitHub personal access token with read access to pull requests (and
  write access when posting reviews) — `GITHUB_TOKEN`
- A provider API key — `ANTHROPIC_API_KEY` (default profile),
  `OPENAI_API_KEY`, `ZAI_API_KEY` (or its `ZHIPUAI_API_KEY` alias), or
  `OLLAMA_API_KEY` for a local server

difftrace depends on loopctl via a git pin until loopctl 0.3.1+ is
published to crates.io (0.3.0 lacks the trajectory capture difftrace
uses); `cargo` resolves it automatically.

## Usage

```
difftrace review --repo owner/repo --pr 42 [--dry-run] [--config PATH]
```

- `--repo owner/repo` — the repository to review (required)
- `--pr N` — the pull request number (required)
- `--dry-run` — render the review to stdout instead of posting it
- `--config PATH` — explicit config file; default `~/.difftrace/config.toml`
  (a missing file there means defaults; a missing explicit path is an error)

The `DIFFTRACE_PROFILE` environment variable overrides
`provider.profile` without a config file — the natural mechanism in CI,
where no `~/.difftrace/config.toml` exists (e.g. `DIFFTRACE_PROFILE=zai`
in a GitHub Action).

Progress goes to stderr; the rendered review (dry run) and the posting
receipt go to stdout. Exit status is non-zero only on error — a review
full of findings still exits `0`.

Posted reviews carry the summary, risks, and test-coverage note as the
body, one inline comment per grounded finding (severity-tagged, anchored
to the head commit), and an HTML-comment block listing any dropped
findings — citations outside the changed hunks or over the per-file cap —
so nothing is silently discarded.

Every run captures a JSONL trajectory under
`~/.difftrace/trajectories/` recording the model requests, tool calls,
and findings as they actually happened; if that directory cannot be
created, difftrace says so on stderr and proceeds without capture.

## Configuration

`~/.difftrace/config.toml` — every field optional:

```toml
[provider]
profile = "anthropic"      # anthropic | openai | zai | ollama
model = "claude-sonnet-4"  # optional; ollama requires it; zai defaults to glm-4.7
base_url = "…"             # optional endpoint override

[github]
api_base_url = "…"         # optional; GitHub Enterprise API root
                           # (e.g. https://github.example.com/api/v3)

[review]
max_findings_per_file = 5  # accepted findings per file (cap receipts both)
batch_files = 4            # changed files per agent run
max_turns = 16             # turn budget per batch (soft stop)
```

API keys and tokens are never read from the config file — environment
only.

## Building and testing

```
cargo build --all-features        # build the binary
make ci                           # fmt, clippy -D warnings, tests, docs
make e2e                          # live dry-run review; requires
                                  # GITHUB_TOKEN, DIFFTRACE_TEST_REPO,
                                  # DIFFTRACE_TEST_PR
```

## Status

Early development. The review pipeline is complete end to end; the
webhook-driven bot, GitHub App auth, and re-review-on-push flows are
next. Not yet published to crates.io — build from source.
