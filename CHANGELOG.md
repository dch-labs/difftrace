# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial crate skeleton: library and binary targets, CI pipeline, style
  configuration (clippy thresholds, strict lint table), and Makefile gates.
  Pinned by `tests/skeleton.rs`.
- Configuration loading (`DifftraceConfig`) from `~/.difftrace/config.toml`
  with typed errors; provider profile selection, `GitHub` endpoint override,
  and review tuning. Secrets are environment-only, never in the file. Pinned
  by the `config::tests` suite (defaults on empty and missing input, unknown
  profile rejection, partial-section defaults, round trip).
- Provider factory (`build_client`) mapping the configured profile onto
  loopctl's Anthropic and OpenAI clients via the statically dispatched
  `DifftraceClient` enum, with the `Ollama` profile riding the OpenAI
  protocol at a local endpoint; keys resolve from the environment only,
  empty strings counting as missing. Pinned by the `provider::tests` suite
  (per-profile construction, missing/empty key errors naming the variable,
  base-URL overrides on both profiles, Ollama model requirement).
- `GitHub` REST layer behind the `PrGateway` trait: pull request summary,
  raw diff fetch, file content at a ref, review-comment listing, and atomic
  review submission (summary + inline comments anchored with `line`/`side`)
  via octocrab. Pinned by the `github::tests` and `github::review::tests`
  suites (wire fixtures through real octocrab models, content decode
  including the above-1-MB rejection, the submission wire body built from
  the submission) and `tests/github_live.rs` behind `DIFFTRACE_E2E=1`.
- Unified-diff model: parser plus `DiffIndex` with `clamp_to_hunk`, the
  authority resolving a cited line to a comment-anchorable line and
  rejecting citations outside the changed hunks. Hunk headers whose declared
  counts the body contradicts fail the parse, as do body lines outside any
  hunk. Pinned by the `diff::parse::tests` suite (side numbering,
  consecutive-hunk renumbering, new/deleted/binary files, `--`-prefixed
  body lines, embedded ` b/` paths, count validation).
- Review tools: five loopctl `Tool` implementations (`get_pr_overview`,
  `get_file_diff`, `read_file_at_head`, `list_review_comments`,
  `submit_review`) over the `PrGateway` seam and the diff index, assembled
  by `ReviewScope::registry`. `submit_review` is the only write-class tool
  and grounds every finding through `clamp_to_hunk` before posting,
  dropping ungrounded findings with a receipt that names each one; it
  fires at most once per run — a second submission is refused before the
  gateway, and a failed submission may be retried. Pinned by the
  `tools::*::tests` suites (rendering including the comment side, gateway
  call mapping, grounding drops, the per-file cap, invalid-input
  rejection, the once-guard and its retry path, registry assembly)
  against an in-memory fake gateway; the tool's findings-item schema is
  composed from `Findings::schema()` and pinned equal to it.
- `DiffIndex::file_section` serving each file's raw unified-diff section
  from the retained source text. Pinned by
  `a_file_section_carries_only_that_files_raw_text`.
- Review engine: `ReviewRunner` (generic over the loopctl client) driving
  one `BareLoop` per batch — rubric `ContextContributor` re-emitting the
  rules and pull-request frame every turn, `record_findings` as the
  batch's single output channel, output-limiting middleware on the tool
  pipeline, a `TrajectoryObserver` capture per run (JSONL to a configured
  directory), clean soft-stop on turn-budget exhaustion, and a
  structured-output summary pass over the aggregated findings; the
  per-file findings cap is enforced at record time with a receipt, and
  findings carrying `line: 0` are rejected where findings enter the
  system. Pinned by the `review::*::tests` suites (findings recorded
  through a scripted `MockApiClient` engine run, budget-exhaustion soft
  stop, trajectory JSONL on disk, summary generation, rubric and
  record-tool contracts, record-time cap receipt, zero-line rejection,
  and the output limit proven by an oversized tool payload carrying the
  `[truncated]` marker in the trajectory).
- Orchestration: `ReviewRunner::review_all` batches the diff's changed
  files (sorted, `batch_files` per batch, size 0 treated as 1), runs each
  batch, aggregates and grounds the findings through the shared
  `ground_findings` (also now the submit tool's engine), summarizes, and
  either posts the review or returns it for a dry run — the posted body
  and the dry-run render are the same `ReviewOutcome::render_markdown`
  output, with dropped findings recorded in both. Pinned by the
  `review::batch::tests` suite (batch planning, a full two-batch dry run
  with grounding drops in the render, and the posted submission asserted
  identical to the dry-run content).
- CLI: `difftrace review --repo owner/repo --pr N [--dry-run] [--config
  PATH]` wiring the whole pipeline — config load, `GITHUB_TOKEN`-backed
  gateway, diff fetch and parse, provider client, trajectory capture to
  `~/.difftrace/trajectories`, and the orchestrated review. Dry run
  renders the review to stdout; posting prints a findings receipt; exit
  is non-zero only on error, never on findings. An explicit `--config`
  path that does not exist is an error, the provider client is built
  before any network call, and disabled trajectory capture says so on
  stderr. Pinned by the `cli::tests` suite (argument parsing,
  `--version`, owner/repo parsing and rejection of malformed forms) and
  binary verification (config guard, flag-form e2e); `make e2e` runs a
  live dry-run review behind
  `GITHUB_TOKEN`/`DIFFTRACE_TEST_REPO`/`DIFFTRACE_TEST_PR`.
- Findings schema: `Findings` and `ReviewSummary` as loopctl
  `StructuredOutput` types with strict JSON Schemas and a closed four-level
  severity set. Pinned by the `findings::tests` suite (schema/serde mirror,
  round trips, unknown-severity rejection).
