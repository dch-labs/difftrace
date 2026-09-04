# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-09-04

### Added

- One verdict comment per pull request, edited in place: the full
  verdict/summary/fix-all body now lives in a single top-level PR
  comment marked with a hidden `<!-- difftrace:verdict -->` header
  (created on the first review, edited on every re-review, footer
  naming the reviewed commit), while the review submission keeps its
  REQUEST_CHANGES / APPROVED event pairing with a one-line pointer
  body. Pinned by `the_verdict_comment_is_created_then_edited_not_duplicated`,
  `the_posted_review_body_is_the_pointer_not_the_verdict`, and
  `verdict_comment_body_wraps_the_render_with_marker_and_footer`.
- Re-raised findings reply into their existing thread: a grounded
  finding whose (path, line) anchor matches an open difftrace thread
  is posted as a reply through the dedicated review-comment replies
  endpoint after the atomic review POST (the create-review request
  schema documents no `in_reply_to`, so the review itself stays
  positioned-comments-only) instead of opening a duplicate comment;
  each reply names the commit that re-raised it, and threads not
  matched resolve as before. Replies are best-effort: one that cannot
  be posted logs a warning and never fails the run — the thread stays
  open for the next re-review, and the finding remains in the verdict
  comment. Pinned by
  `a_re_raised_finding_replies_into_its_thread`,
  `a_failed_reply_degrades_without_failing_the_run_or_resolving`,
  `reply_matching_prefers_the_current_line_then_the_original`, and
  `a_shifted_finding_posts_a_new_comment_and_resolves_the_old`.

### Fixed

- Thread resolution now works under GitHub App installation tokens:
  the thread listing learned its own login via REST `GET /user`, which
  installation tokens may not call — every production run logged
  "could not list previous review threads" (Resource not accessible
  by integration) and resolved nothing. The login now comes from the
  GraphQL `viewer` query, valid for PATs and installation tokens
  alike. Pinned by `the_login_comes_from_the_graphql_viewer_query`
  and `live_own_open_threads_lists_under_the_token`, which `make e2e`
  runs with `DIFFTRACE_E2E=1` ahead of its live dry-run.

### Changed

- Posting the verdict comment is retried (three attempts, one second
  apart) and fails the run loudly when every attempt fails — the
  review, thread replies, and resolutions complete first, so a red
  verdict build defers nothing but the verdict itself; the next
  re-review heals it. Pinned by
  `a_verdict_upsert_exhausting_the_retries_fails_the_run_after_posting`
  and `a_verdict_upsert_recovering_on_a_later_retry_succeeds`.
- `PrGateway` grows `find_own_marker_comment` and `update_issue_comment`
  (breaking for external implementors; pre-1.0 minor bump when cut).

## [0.2.1] - 2026-09-04

### Added

- Comment commands: `difftrace reply --repo o/r --pr N (--issue-comment
  ID | --review-comment ID)` answers a question asked in a comment —
  inline in the same review thread when asked under a finding, as a
  top-level PR comment @mentioning the asker for conversation
  questions. Authorization lives in the binary: collaborators
  (admin/write) and the PR author may invoke; anyone else gets a
  one-line refusal posted the same way. The reply run is one agent
  loop with the finding, its file's diff section, and head content as
  context (a `ReplyRubric` contributor re-emits the chat rules each
  turn), bounded by the new `review.reply_max_turns` setting (default
  8), trajectory captured like every run. `@difftrace re-review` needs
  no binary path — consumer workflows' new `chat` job parses the verb
  and re-runs the review. `PrGateway` gains the comment surface
  (fetch issue/review comment, collaborator permission, thread reply,
  top-level comment). Pinned by the reply suite (in-thread vs
  top-level targeting, refusal, PR-author authorization, turn-budget
  exhaustion) and the CLI parse tests (exactly one comment kind).

### Fixed

- Fatal errors and thread-resolution warnings print the full error
  chain — octocrab's error type displays only a variant name ("GitHub"),
  so the actual rejection message and documentation URL sat two
  `source()` levels down and never reached the log. Pinned by the
  error-chain unit test.

## [0.2.0] - 2026-09-04

### Added

- The review is submitted as the GitHub review event matching the
  verdict — `REQUEST_CHANGES` while blockers exist, `APPROVED` when
  clean (note an app approval can satisfy branch-protection
  required-review counts). Pinned by the wire and event-pairing tests.
- Re-review thread resolution: on every posted review, previous threads
  authored by the reviewing identity resolve when no new finding lands
  on their file and anchor line — outdated threads with no current
  anchor resolve too. Threads come from the GraphQL `reviewThreads`
  connection (cursor-paginated past the first 100), filtered by the
  `/user` login of the token, and resolve through the
  `resolveReviewThread` mutation after the review posts;
  thread-listing and per-thread failures log a warning and never fail
  the review. Pinned by the gateway wire fixture, the resolution-slate
  unit, and a full scripted re-review.
- Visual severity language: inline comment headers carry a shields.io
  severity badge (`nitpick` grey, `suggestion` blue, `warning` orange,
  `critical` red — rendered by that third-party service with alt text
  as fallback), while the verdict blockers and the fix-all report use
  severity glyphs (💬 💡 ⚠️ 🔴); the copyable agent prompts keep the
  plain `[severity]` word. Pinned by the severity ladder test and the
  render asserts.
- Per-finding fix complexity: the model rates every finding 1–5
  (1 a one-liner, 5 needs restructuring; taught in the rubric, required
  in the findings schema, out-of-range values rejected at both findings
  entry points). Comment headers carry a second badge on a color ramp
  (1 blue, 2 green, 3 yellow, 4 orange, 5 purple — red stays reserved
  for critical severity), verdict blockers and fix-all report lines
  carry the matching circle glyph in parentheses, and the copyable
  prompts carry a plain `Complexity: n/5` line. Pinned by the ladder
  test, the entry-rejection tests at both gates, and the render
  asserts.

### Changed

- The structured summary pass now makes two corrective retries (three
  attempts total) when the returned JSON fails schema parsing,
  feeding each parse error back to the model; exhaustion and
  final-attempt recovery are pinned.

## [0.1.0] - 2026-09-03

First working release: the full review pipeline end to end — diff
grounding, batch agent runs, typed findings, summary, atomic posting,
dry run, CLI. Depends on loopctl via a git pin until loopctl 0.3.1+
reaches crates.io; the switch is a drop-in once it does.

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
  empty strings counting as missing. The `zai` profile rides loopctl's
  Anthropic-compatible client at `https://api.z.ai/api/anthropic`
  (default model `glm-4.7`, key from `ZAI_API_KEY` or its
  `ZHIPUAI_API_KEY` alias, empty strings counting as missing on either
  side of the fallback). Pinned by the `provider::tests` suite
  (per-profile construction, missing/empty key errors naming the variable,
  base-URL overrides on both profiles, Ollama model requirement; the zai
  suite pins the endpoint, default model, alias fallback, and model
  override).
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
  structured-output summary pass over the aggregated findings, sent with
  `strict` off because Anthropic-protocol endpoints (the default profile
  and zai) refuse strict response formats at request time, with one
  corrective retry when the returned JSON fails schema parsing; the
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
  is non-zero only on error, never on findings. The
  `DIFFTRACE_PROFILE` environment variable overrides the configured
  profile without a config file, and `DIFFTRACE_MODEL` overrides the
  configured model the same way — empty values counting as unset
  (pinned by the `config::tests` override suite alongside
  profile-string parsing and rejection). An explicit
  `--config`
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
- `tracing` logging end to end — stage boundaries, per-batch start/finish
  with file lists and finding counts, a `LoggingObserver` mirroring run
  turns and tool calls into the log, and the summary retry warning —
  controlled by `RUST_LOG` (default `info`).
- Binary releases from the `Binaries` workflow: a rolling `nightly`
  prerelease on every master push, plus a release carrying the same
  linux x86_64 tarball for every `v*` tag. Actions download a prebuilt
  binary pinned to an audited tag instead of compiling the repository's
  default branch, so upstream branch changes cannot alter the privileged
  review binary; the review workflow in consumer repos fetches it with
  `gh release download` and uploads the run's trajectory JSONL as an
  artifact.
- Agent fix prompts in the posted review, all rendered from one wording
  source (`prompts`): every inline finding carries a collapsed
  "🤖 Fix prompt" section whose fenced block (GitHub's copy button)
  holds a self-contained instruction — file, anchored line, severity,
  title, detail, and fix directives — and the summary body gains a
  "Fix all findings" section pairing a readable per-finding report
  with a copyable prompt naming the pull request and head commit.
  Findings dropped during grounding join the fix-all prompt marked
  unanchored with their drop reason: still never posted as inline
  comments, but no longer invisible to fix-it passes. Pinned by the
  `prompts::tests` suite plus the grounding and render tests.
- Verdict section leading every rendered review: good to go exactly
  when no grounded finding carries warning or critical severity —
  blockers are listed with file and line, unanchored (dropped) findings
  never block, and the "to be good to go" note points at the fix
  prompts. Derived mechanically from the posted findings, so the verdict
  cannot contradict them; the risks section is always rendered
  ("(none flagged)" when empty). Pinned by the batch render suite.
