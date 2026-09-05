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
in a GitHub Action). `DIFFTRACE_MODEL` overrides `provider.model` the
same way; an empty value counts as unset, and without either the
provider's default model applies (zai: `glm-4.7`).

Comments starting with `@difftrace` or `/difftrace` trigger commands
in repos whose workflows listen for them: `re-review` re-runs the full
review, and anything else asks a question — under a finding, the
answer lands in that thread; on the PR conversation, it lands as a
comment mentioning the asker. Collaborators and the PR author may
invoke; others get a refusal. Chat answers are bounded by
`review.reply_max_turns` (default 8).

Progress goes to stderr; the rendered review (dry run) and the posting
receipt go to stdout. Exit status is non-zero only on error — a review
full of findings still exits `0`.

Each pull request carries one standing difftrace comment — marked with
a hidden `<!-- difftrace:verdict -->` header, created on the first
review and edited in place on every re-review, with a "Reviewed
commit" footer naming the round's head SHA. It is a cross-round issue
registry: every issue difftrace has ever raised on the pull request,
kept as hidden JSON inside the comment and merged each run. The
Verdict section lists all unresolved issues, blockers first, each
with severity and effort badges — good to go exactly when no
unresolved issue is a warning or critical, and unanchored (dropped)
issues count as blockers too. Fixed issues close with the round that
fixed them and render in a bottom "✅ Issue history" section; threads
resolved outside difftrace record "manually resolved". (Overlapping
runs on the same PR can create a second marker comment; later runs
edit the newest.) The registry is followed by the summary, the risks
section (always present, "(none flagged)" when empty), and the
test-coverage note.
Each round's review submission carries the matching `GitHub` review
event — requesting changes while unresolved blockers exist, approving
when clean, one source of truth with the standing verdict — with a
body leading on a stat line ("🤖 difftrace reviewed `abc1234`
— 4 findings this round; fix prompts below.") and that round's fix-all
prompt, plus one inline comment per grounded finding (each headed by a colored
severity badge plus a fix-complexity badge on a 1–5 color ramp,
anchored to the head commit) — each with a collapsed "🤖 Fix prompt"
section whose fenced block has a copy button. The fix-all report
covers every raised finding, naming the pull request and head
commit. Findings dropped during grounding — citations outside the
changed hunks or over the per-file cap — join the registry marked
unanchored and appear in the fix-all prompt, so nothing is silently
discarded.

On a re-review, a finding raised again at the same anchor is posted as
a reply into its existing thread — each reply naming the commit that
re-raised it — instead of opening a duplicate, and previous difftrace
threads whose finding did not reappear (fixed, dropped, or shifted to
a new line) are resolved automatically. Writing the verdict comment is
retried and fails the run loudly if every attempt fails, so a missing
verdict is visible rather than silent.

Every run captures a JSONL trajectory under
`~/.difftrace/trajectories/` recording the model requests, tool calls,
and findings as they actually happened; if that directory cannot be
created, difftrace says so on stderr and proceeds without capture.

## The consumer workflow

[`examples/difftrace-review.yml`](examples/difftrace-review.yml) is the
reference `GitHub` Actions workflow for running difftrace on a
repository — reviews on every pull request, `@difftrace`/`/difftrace`
comment commands through a first-line mention, and per-PR concurrency
that never lets a comment cancel an in-flight review. Copy it verbatim;
on every difftrace release, bump the two version+checksum pins (both
install steps). Setup it assumes: the difftrace `GitHub` App installed with
`pull-requests: write` and `contents: write` (conversation resolution
requires repo-write on the token — `contents: read` leaves
`resolveReviewThread` rejected), the `ZAI_API_KEY`,
`DIFFTRACE_APP_ID`, and `DIFFTRACE_APP_PRIVATE_KEY` secrets, and —
optionally — a `DIFFTRACE_MODEL` repository variable to steer the model
away from the default.

## Configuration

`~/.difftrace/config.toml` — every field optional:

```toml
[provider]
profile = "zai"      # anthropic | openai | zai | ollama
model = "glm-4.7"  # optional; ollama requires it; zai defaults to glm-4.7
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
