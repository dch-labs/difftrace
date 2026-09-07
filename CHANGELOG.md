# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.11.0] - 2026-09-07

### Changed

- `review.batch_files` and a new `review.max_parallel_batches` are both
  overridable from the environment (`DIFFTRACE_BATCH_FILES`,
  `DIFFTRACE_MAX_PARALLEL_BATCHES`, each validated to at least 1), and
  batches can review concurrently, bounded by the lane count (default 1,
  the previous sequential behavior; aggregate order stays batch order
  regardless of completion order). The reference workflow sets
  `DIFFTRACE_BATCH_FILES=100`, so a pull request reviews as one batch in
  one agent loop (pinned by
  `parallel_lanes_review_every_batch_and_keep_none_unreviewed`,
  `the_parallel_batches_env_override_reaches_the_review_settings`, and
  `the_batch_files_env_override_reaches_the_review_settings`).

## [0.10.0] - 2026-09-07

### Added

- Every run names the provider profile and model it resolved at startup
  (`provider client built` on the `difftrace::provider` target), so the
  run log states which model reviewed the code instead of leaving it
  implicit in the workflow env (pinned by
  `the_built_client_logs_its_profile_and_model`).
- The caller template's header lists the guards the pinned callee
  enforces — fork and dependabot rejection, collaborator authorization,
  secret preflights, the checksum-pinned binary, least-privilege app
  token — so consumer-PR reviewers can verify them without reading the
  callee (workflow-only).

### Changed

- The review workflow splits the per-PR concurrency lane: reviews run in
  their own lane with cancellation, so a push supersedes the in-flight
  review instead of queueing a duplicate of a stale head (flagged
  independently by two reviewers on dch#25); comment commands keep a
  serialized no-cancel lane, except a `review` command, which joins the
  review lane so the latest intent cancels the in-flight review. A
  review and a non-review command may still interleave — accepted in
  exchange for never reviewing a stale head twice (workflow-only; rides
  the next release chore with the consumer pin bumps).
- Every difftrace invocation passes `--sha` with the head resolved in
  the workflow's first step, binding the review, the command, and the
  memory-cache key to one commit (workflow-only; the recorded v0.9.0
  follow-up).
- The command gate declines bot-authored comments in the job condition
  before any setup, so the gate runs a review used to spawn for its own
  inline comments no longer happen (workflow-only).

## [0.9.0] - 2026-09-07

### Added

- The review workflow ships as a reusable workflow at
  `.github/workflows/review.yml`, dogfooded on this repository's own
  pull requests; consumers call it through the small caller template at
  `examples/difftrace-review.yml`, pinned to the fixed commit that
  `release.sh pins` echoes — workflow fixes now propagate by bumping
  one line per consumer instead of re-copying the file. Verbatim copies
  keep working; migrate them at the next release chore. `release.sh`
  learned the new layout (workflow pins in `.github/workflows/`,
  the template pinned to the pins commit).
- `--sha <commit>` on `review`, `reply`, and `plan`: the run fails before touching
  anything unless the pull request's current head is exactly the requested
  commit, so a consumer can bind the run and its memory-cache key to one
  SHA instead of racing a push between its resolve step and the tool's own
  (pinned by `a_sha_pin_matching_the_head_is_accepted` and
  `a_sha_pin_off_the_head_fails_and_names_both_commits`). The pin is
  re-checked after the diff is fetched, so a push landing mid-fetch fails
  the run instead of reviewing a newer diff under the older pin (pinned
  by `a_head_that_moves_mid_fetch_fails_the_pinned_run`). The reference
  workflow passes `--sha` only once the release chore bumps its pins to a
  binary that supports it — that wiring is part of the next release chore.

### Changed

- Tool outputs are truncated at 128k characters instead of 16k: the old
  cap cropped `read_file_at_head` mid-file, so a fix landing beyond the
  crop was verified against stale content and raised again — six of
  eleven findings on loopctl#112 round 5 were such re-reports. The bound
  stays pinned by `an_oversized_tool_output_is_truncated_for_the_model`,
  which scales with the constant.
- The review rules direct the model at the batch's evidence pack as the
  primary source, fetching with the tools only what the pack does not
  show — rounds were spending several fetch turns per batch on content
  the pack already carried (pinned in
  `the_rubric_carries_rules_and_frame_every_turn`).

### Fixed

- The reference workflow drops the repo-level memory restore key: a new
  pull request no longer restores another PR's learnings file and
  reviews under it (workflow-only; found by difftrace on loopctl#112).
  Rides the next release chore with the pin bumps.
- A `plan` command on a pull-request conversation posts a visible hint
  comment — naming `@difftrace review` for a fresh full review — and
  runs the generic reply it previously ran silently (workflow-only).
  Rides the next release chore.
- The review job's concurrency comment states the
  one-running-one-pending lane semantics, so a command dropped by a
  third arrival is diagnosable from the workflow instead of mysterious
  (workflow-only). Rides the next release chore.

## [0.8.0] - 2026-09-07

### Added

- `review.stream_timeout_secs` (env `DIFFTRACE_STREAM_TIMEOUT_SECS`,
  default 900) sets the total streaming budget per model turn. The
  previous hard-wired 300 seconds killed a thinking model's largest
  batches mid-generation — with the output budget raised, the time
  limit had become the binding constraint — and the doomed retry then
  burned the same seven minutes again before the batch landed in the
  unreviewed path. The reference workflow ships 900.

## [0.7.0] - 2026-09-07

### Added

- `provider.max_tokens` (env `DIFFTRACE_MAX_TOKENS`) sets the
  per-response output budget on Anthropic-protocol providers; the
  reference workflow ships 32768 and takes effect with this release —
  earlier binaries ignore the variable and run at the provider
  default, so the workflow's version+checksum pins must be bumped in
  the same release. The provider default of 8192 is
  unusable for thinking models — a model can burn the whole budget on
  reasoning before emitting anything usable.

### Changed

- A batch whose final turn reaches the output budget, or that ends
  without a single `record_findings` call, fails into the retry path
  and lands in the "⚠️ Unreviewed files" section — a truncated think
  can no longer masquerade as a clean review, and such a round
  withholds approval and records `(incomplete)` memory instead of
  approving (pinned by `a_truncated_batch_is_recorded_unreviewed_instead_of_approved`,
  `a_final_turn_truncated_at_the_output_budget_fails_the_batch`, and
  `a_run_out_of_turns_without_a_verdict_is_not_a_clean_review`).
- Evidence packs: lockfiles and generated files render as a one-line
  change summary instead of thousands of windowed lines, and no single
  file may take more than half the pack — one fat file no longer
  starves every file after it (pinned by
  `a_lockfile_is_summarized_not_inlined` and the share assertions in
  `the_joined_pack_stays_under_the_total_cap`).
- The `list_review_comments` tool drops bot-authored comments and
  truncates human bodies to bounded excerpts — another reviewer bot's
  embedded tool output can no longer ride the whole conversation
  (pinned by `bot_comments_are_dropped_and_human_bodies_stay_bounded`).
- Each batch's cross-round history is scoped to its own files, so an
  open issue on another batch's file no longer drags a batch off its
  files to chase context (pinned by
  `the_batch_history_only_names_the_batchs_own_files`); the miss hunt
  skips batches the first pass could not complete (pinned by
  `the_hunt_skips_batches_the_first_pass_could_not_complete`).
- The verdict comment is upserted before the review is submitted, so
  it sits above the round's review entry and its inline comments on
  the conversation timeline; a verdict write that cannot land warns
  and falls open instead of blocking the review (pinned by
  `the_verdict_comment_goes_up_before_the_review_is_submitted` and
  `a_verdict_comment_that_cannot_be_written_does_not_block_the_review`).

## [0.6.0] - 2026-09-06

### Added

- Cross-review memory: `difftrace review --memory <file>` loads a
  project-memory file into the reviewer's rubric (quoted as data) and
  appends a per-commit learning section after each posted review — which
  commit, clean or findings, raised and fixed titles. The reference
  workflow persists the file with `actions/cache` keyed by repository
  and pull request, so the reviewer remembers previous rounds and
  previous PRs (pinned by `load_returns_none_for_a_missing_or_empty_file`,
  `append_prunes_and_keeps_the_newest_sections`, and
  `a_learning_section_names_the_commit_state_and_titles`).
- Evidence packs: every batch's prompt now carries the final content of
  each changed file at the reviewed commit — full for files that fit
  the cap, ±40-line windows around the hunks for large ones — quoted
  as untrusted data, so the reviewer reasons over the real file instead
  of choosing whether to fetch it (pinned by
  `a_small_file_rides_in_full`,
  `a_large_file_becomes_windows_around_its_hunks`, and
  `an_unreadable_file_degrades_to_a_note`).
- A miss-hunt second pass: when the first pass records no blocking
  finding, the batches re-run with an adversarial framing (lifecycle
  transitions, unchanged or test code the change activates, terminal
  actions on the wrong path) and any surviving findings ride the normal
  verification and grounding pipeline. Disable with
  `review.miss_hunt = false` (pinned by
  `a_clean_first_pass_triggers_the_miss_hunt`,
  `a_blocking_first_pass_skips_the_miss_hunt`, and
  `the_hunt_rubric_argues_against_a_clean_first_pass`).
- A hunt finding that repeats a first-pass finding (same file, line,
  and title) is dropped instead of double-counting the round and the
  fix-all prompt (pinned by
  `a_hunt_finding_repeating_the_first_pass_is_dropped`).
- A run that posts the review but then errors records an
  "(errored)" memory section, so the memory file reflects the round
  even when the verdict write failed.

### Changed

- Cross-review memory records an incomplete round as `(incomplete)`
  with the unreviewed file list, never as clean — a round that could
  not read every file does not teach the next round that the change
  was read (pinned by
  `a_learning_section_names_the_commit_state_and_titles`).
- Batch resilience: a reviewer run that cannot complete a batch
  (provider outage, stream timeout) retries the batch once, then
  records it in a visible "⚠️ Unreviewed files" section of the standing
  comment instead of failing the whole review — the batches that did
  complete still post their findings and verdict. A round with
  unreviewed files never posts approval: the review is submitted as a
  neutral `COMMENT` with an "Approval withheld" verdict, the round
  body drops the clean-round wording, and the memory section records
  the round as incomplete (pinned by
  `a_round_with_unreviewed_files_does_not_post_approval`).
- `ReviewSettings` no longer implements `Copy` (it now carries the
  runtime memory content) — source-breaking for downstream `Copy`
  users; rides the next minor bump.
- Two reviewer rubric rules: lifecycle outcome typing (trace every
  stop/shutdown path end to end — which branch returns, which terminal
  action fires; a normal stop never shares a terminal action with a
  forced exit) and test-code grading (test code that can kill
  processes, race signals, or corrupt the environment is
  warning-grade).

## [0.5.0] - 2026-09-06

### Added

- Chat depth: replies and plans now see the earlier turns of their
  review thread (up to the last 20, each body capped) or the recent
  PR-conversation trail (last 10, bodies capped), so follow-up
  questions continue the discussion instead of starting from the
  finding alone. Gathered comment text is rendered into the prompt as
  quoted, untrusted data — never as instructions. This grows the
  `PrGateway` trait with `issue_comments` (breaking for external
  implementors; pre-1.0 minor bump when cut) — pinned by
  `a_thread_reply_carries_the_earlier_turns`,
  `a_conversation_reply_carries_the_recent_trail_without_the_target`,
  and `the_issue_comments_listing_targets_the_newest_first_page`.
- `@difftrace plan` under a finding produces a step-by-step fix plan
  for it — numbered steps naming files and lines, how to verify, and
  risks — from a planning-specific prompt that reads the code but
  never claims to change it. The plan posts with a collapsible
  "🤖 Plan prompt for coding agents" copy block (verify-first, like
  the fix prompts), so it pastes straight into an agent. On the PR
  conversation `plan` degrades to a question; the CLI rejects a
  conversation-targeted `plan` with a hint (pinned by `a_thread_reply_carries_the_earlier_turns`,
  `a_plan_prompt_names_the_finding_and_asks_for_steps`,
  `a_plan_command_posts_into_the_thread`,
  `a_plan_on_the_conversation_is_rejected_with_a_hint`, and
  `a_plan_without_a_readable_finding_fails_closed`). The workflow's
  `plan` routing requires the release pin bump to a binary that has
  the subcommand — merge rides the release.

- The reviewer now reviews with memory: every batch's rubric carries
  the registry's cross-round context (still-open issues and the fix
  history with their commits), with the rule that open issues must be
  re-reported at the same location with the same title while a
  suggestion reversing an earlier round's fix needs explicit
  justification (pinned by
  `the_rubric_carries_the_cross_round_history_when_set`,
  `the_cross_round_section_lists_open_and_fixed_issues`, and
  `the_rubric_history_adds_the_re_raise_rules_to_the_bare_list`).
- A verification pass after the batches cross-examines every recorded
  finding and drops the ones that fail — self-refuting findings,
  unverifiable factual claims, reversals of settled fixes, or
  complaints the code's own documentation already answers. Dropped
  findings land in a visible "Dropped after verification" section with
  their reasons; they never reach the fix prompts or the registry. A
  verifier that cannot complete keeps every finding. Disable with
  `review.verify_findings = false` (pinned by
  `findings_failing_verification_are_dropped_with_a_reason` and
  `a_persistently_failing_verification_keeps_every_finding`).
- A hardened reviewer rubric: no findings whose own analysis concedes
  the code is deliberate or needs no change, unverifiable cross-repo
  claims must be marked as assumptions, and documented rationale must
  be engaged with rather than re-flagged (pinned by
  `the_rubric_carries_rules_and_frame_every_turn`).
- Same issue, several locations: findings whose titles match share one
  inline comment anchored at the first location, with the other spots
  listed under "Also occurs at" (in the comment, the fix-all report,
  and the prompt); the registry still tracks every location
  individually. The review tool now asks the model to reuse the exact
  same title and severity when the same issue occurs at several
  locations (pinned by
  `same_titled_findings_share_one_comment_listing_every_location`,
  `same_titled_findings_share_one_fix_all_item`,
  `a_grouped_finding_replies_once_and_registers_every_location`, and
  `a_grouped_secondary_keeps_its_matching_thread_alive`: a location
  whose thread recorded the same issue keeps that thread alive through
  a grouped re-raise).
- Fix prompts are addressed to coding agents: the collapsed section is
  "🤖 Fix prompt for coding agents", and every prompt (per-issue and
  fix-all) opens with a verify-first instruction — check the issue
  still exists at the named location and say so instead of changing
  anything if it is already fixed — so re-pasting an old comment into
  an agent is safe (pinned by
  `a_comment_body_carries_the_finding_and_a_collapsed_fix_prompt`).

### Changed

- A clean round's review body is a single stat line — the pointer to
  the standing comment is dropped when there is nothing to fix (pinned
  by `a_clean_round_body_is_a_single_stat_line_without_the_pointer`).
- The reviewer rubric gains a lifetime rule for runtime-managed
  resources: consider events arriving while a signal listener, watcher,
  timer, or channel is recreated across handoffs (pinned by
  `the_rubric_carries_rules_and_frame_every_turn`).

- Long lines inside the copyable prompt blocks now word-wrap at 80
  columns with a hanging indent; the fix-all items put each finding's
  detail on its own wrapped line instead of one very long line (pinned
  by `prompt_lines_wrap_at_eighty_columns_with_a_hanging_indent`).
- A new finding raised at an anchor whose open thread records a
  different issue now resolves the old thread and opens a fresh one,
  and the registry records the replaced issue as fixed in that round
  (with the fixing commit) instead of overwriting it — previously a
  same-line collision re-raised the thread with the new finding's
  body, so the replaced issue never appeared as fixed. A finding whose
  title still matches the recorded issue re-raises into its thread as
  before; unknown or unparseable titles keep the old behavior.
  Retirement runs after every reply-match of the round, so a re-raised
  issue keeps its thread and its history even when a different issue
  lands on the same anchor in the same review (pinned by
  `a_different_finding_at_a_threads_location_retires_it`,
  `a_mismatched_thread_at_a_secondary_location_is_retired`,
  `a_re_raised_issue_is_not_retired_by_a_different_issue_at_its_anchor`,
  `merge_records_the_replaced_issue_fixed_when_its_thread_is_retired`,
  and
  `a_different_finding_at_the_same_line_resolves_the_old_thread_and_opens_a_new_one`).
- The comment command for re-running a review is `@difftrace review`
  (was `re-review`) — shorter to type, same behavior: the chat job's
  verb parser matches `review` as the first word after the mention
  (workflow-only; no code pins it).

## [0.4.1] - 2026-09-05

### Fixed

- Review-round hardening of the registry: the review event now derives
  from the merged registry's unresolved blockers — one source of truth
  with the standing verdict, so an unanchored blocking finding still
  requests changes (pinned by
  `an_unanchored_blocking_finding_still_requests_changes`); a verdict
  comment that cannot be read fails the run before any write rather
  than resetting the registry; `>` is escaped in the embedded JSON so
  file names and titles containing `-->` cannot truncate it
  (`the_embedded_registry_survives_arrows_in_titles_and_files`); a
  finding re-flagged with an out-of-hunk citation keeps its anchored
  issue open instead of churning fixed→re-raised
  (`a_dropped_reflag_keeps_the_anchored_issue_open`); and an issue's
  thread identity refreshes on every re-raise so a later fix records
  `fixed in round N` rather than a stale `manually resolved`
  (`a_thread_match_refreshes_the_stale_thread_id`). The round-body
  stat line counts every raised finding of the round, grounded and
  unanchored.

## [0.4.0] - 2026-09-05

### Changed

- Each review round's body now leads with a stat line ("🤖 difftrace
  reviewed `abc1234` — 4 findings this round; fix prompts below.";
  clean rounds read "clean round, nothing to fix") and carries **that
  round's** fix-all prompt — the copy-all block moves off the standing
  comment, which becomes a cross-round issue registry. Pinned by
  `the_round_body_is_a_stat_line_with_the_rounds_fix_all` and the
  round-body integration asserts.
- The standing comment is a registry of every issue difftrace has ever
  raised on the pull request: a hidden JSON state block inside the
  marker comment is merged every run (open issues carry across rounds;
  threads that no longer match close as `fixed in round N (sha)`;
  threads resolved outside difftrace record `manually resolved`;
  unanchored findings join marked unanchored), the Verdict lists all
  unresolved issues blockers-first with severity and effort badges,
  and fixed issues render in a bottom "✅ Issue history" section
  ordered by fix time. PRs with pre-existing difftrace threads
  bootstrap the registry once from their own comment headers. Pinned
  by the registry merge suite, the bootstrap parser pins, and
  `two_rounds_leave_a_registry_with_open_and_fixed_issues`.

## [0.3.3] - 2026-09-05

### Changed

- Review bodies no longer carry the verdict: each round's submission
  body is one neutral line naming the reviewed commit, making the
  standing marker comment the only verdict surface (verdict, summary,
  risks, tests, and the copyable fix-all prompt, edited per run) —
  the CodeRabbit comment model, replacing the verdict-on-review shape
  whose per-round immutable bodies multiplied verdict-looking posts in
  the timeline. Pinned by `the_posted_review_body_is_one_neutral_round_line`
  and `the_round_body_is_one_neutral_line_naming_the_commit`.

### Fixed

- The reference consumer workflow hardens its trust boundary and
  command scheduling: the fork gate fails closed (only an explicit
  `false` passes — `true`, `null`, or a missing value no longer reach
  the credential-minting steps), manual `workflow_dispatch` runs gain
  the same fork rejection the push path already had, and each comment
  command runs in its own concurrency group so a newer pending command
  can no longer evict an older one. Promoted from the maintainer's dch
  fix after CodeRabbit flagged the first two on the consumer copy.

- Thread resolution works under the app token: `resolveReviewThread`
  is gated on repo-write access, so an installation token scoped to
  `contents: read` is rejected with "Resource not accessible by
  integration" even though every other operation (review submission,
  replies, comments) succeeds — the canonical consumer workflow now
  scopes its minted token to `contents: write`, and the App itself
  must carry Contents: Read and write (installation re-approval
  required after changing it). The accumulated stale threads on both
  consumer PRs were resolved manually to verify the mutation under a
  sufficiently permissioned token.

## [0.3.2] - 2026-09-05

### Added

- A reference consumer workflow: `examples/difftrace-review.yml` is now
  the canonical `GitHub` Actions file for running difftrace on a
  repository — the consumer copies in the loopctl and dch repos are
  regenerated from it verbatim on every pin bump, converging drift that
  had grown between the two. It carries the union of their hardening: commit-SHA-pinned
  third-party actions, a scoped app token, first-line-only mention
  matching (quoted mentions never fire), per-repo collaborator
  authorization in a separate gate job, hidden-file trajectory upload,
  and split concurrency groups so a comment command can no longer
  cancel an in-flight review (`difftrace-review-*` cancels on new
  pushes; `difftrace-chat-*` serializes). README documents it as the
  copy-verbatim source with the release pin-bump procedure.

- Transient provider failures now back off and retry instead of failing
  the run: the review and reply agent loops carry loopctl's production
  `StreamHandler` (three transport retries with jittered backoff; five
  rate-limit retries honoring the server's `Retry-After`), replacing the
  zero-retry passthrough handler the engine defaults to — the failure
  mode where a provider 429 killed a run in under a second ("stream
  failed: rate limit, 0 events processed"). The summary pass remains
  fail-fast (one direct request per run; model fallback and budget
  integration are the cascade task's scope). Pinned at both call sites
  by behavioral tests over a fail-first client wrapper —
  `a_transient_stream_failure_is_retried_by_the_production_ladder` and
  `a_transient_stream_failure_is_retried_in_the_reply_path` — each of
  which fails if its loop is reverted to the engine's zero-retry
  default construction, alongside the config-shape pin
  `the_production_managers_carry_a_real_retry_ladder`.

### Changed

- Each review's body now carries that round's verdict — the event-colored
  blocker list — plus a pointer line, so the newest timeline item always
  shows the round's decision; the single marker comment keeps the full
  summary, risks, tests, and fix-all prompt, edited in place (the
  pointer-only body put the summary below the pointer in the
  conversation and wasted the most prominent per-round surface). Pinned
  by `the_posted_review_body_carries_the_verdict_and_points_at_the_comment`.

### Fixed

- Own-thread and marker-comment matching tolerates GitHub's bot-login
  inconsistency: the GraphQL `viewer` for an app token reports
  `name[bot]`, while GraphQL thread-comment authors (and some REST
  surfaces) report the unsuffixed `name` — strict comparison matched
  zero threads, so the first 0.3.1 run logged `replied=0, resolved=0`
  despite a successful listing. Both comparisons now normalize away a
  trailing `[bot]`. Pinned by the rewired wire fixtures (authors in
  the unsuffixed GraphQL form) and the marker-finder suffix case.

## [0.3.1] - 2026-09-05

### Fixed

- Clean reviews post again: the create-review request enum for an
  approving review is `APPROVE`, but difftrace sent `APPROVED` (the
  response state) — the first live clean review under 0.3.0 was
  rejected wholesale ("Variable $event … invalid value"). Pinned by
  `review_events_map_to_their_wire_names`, which now states the
  request/response distinction.
- The thread-listing GraphQL query is well-formed: its string
  constant used Rust backslash line-continuations, which strip the
  newline and the next line's indentation — `id`, `isResolved`, and
  `comments` collapsed into the nonexistent field
  `idisResolvedcomments` and the query 422'd ("Field … doesn't exist").
  The bug predates 0.3.0 and was masked until the viewer-login fix
  let the query run for the first time. All GraphQL documents are now
  raw strings with real newlines, and
  `the_graphql_documents_keep_every_field_a_separate_token` fails on
  any future mash.

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
