# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased — 0.12.0-alpha.1, the rewrite branch]

### Added

- **Parity gate tooling** — `eval-run --parity --v1 <metrics.json>
  --current <metrics.json>` renders `eval/reports/current-vs-v1.md` (the M1–M5
  comparison with the v1 baselines inline) and evaluates the release
  gates: any miss — including an unmeasured metric, which fails
  closed — names its owning task and fails the run, so the cutover
  cannot ride on absent data. The report ingestion
  (`parse_current_report`) maps confirmed and solo findings by
  their first evidence cite into the eval finding shape (weak leads
  stay out of the recall signal); `make eval-parity` runs the
  comparison over the conventional metrics paths. Pinned by the
  report suite (both runner rows render, misses name owners,
  unmeasured metrics fail closed, the cost gate halves exactly,
  report ingestion). The measured runs themselves stay owner-gated
  spend (model-spend corpus runs are deliberately behind explicit
  targets, never a default), as do the suppression seeds, the
  CodeRabbit planka, and the sign-off.

- **Current runner in the eval harness** — `eval-run --mutations --runner
  current --binary <difftrace>` drives the rewrite binary over the mutation
  corpus and harvests the run's `<checkout>-*.report.json` (confirmed
  and solo findings by first evidence cite) instead of v1 transcripts;
  the v1 harvest now ignores report files in mixed review dirs, and
  `make eval-run-current` points at `target/release/difftrace`. Verified
  end to end offline: a stub reviewer carrying a case-matching
  finding scores `hit=true` through the real apply→run→harvest→report
  path with zero model spend (`the_current_harvest_takes_only_new_reports`
  pins the harvest; the run itself is the process-level check).

- **Release train with a moving tag** — consumer callers pin
  `dch-labs/difftrace/.github/workflows/review.yml@v0.12` (bump
  major/minor only): the rewritten reusable review workflow installs
  the binary from the moving tag and runs the hosted review with a
  least-privilege app token, fork and bot guards, and first-line
  `/review` comment triggers. `release.yml` owns every `v0.12.*` tag
  push: build, publish the exact-tag release, then move the `v0.12`
  tag in CI (never from a local tree); `binaries.yml` is nightly-only.
  `release.sh` keeps the cut (gate → commit → tag → push) and gains
  `--check`, which validates the plan — version, exact tag, moving
  tag, changelog section — without committing, tagging, or pushing.
  The moving tag replaces the v1 in-file checksum pin; the trust
  boundary is tag protection on this repository (owner-side setting)
  plus the printed sha256 in every install log. Pinned by
  `tests/workflows.rs` (the review workflow contract, the CI tag move, the
  caller pin with its bump comment, and both `--check` outcomes).

- **Hosted mode** — `difftrace review --repo owner/name --pr N --actor
  <login>` with `GITHUB_TOKEN`, run from a checkout of the PR head:
  the same core as local mode over a `GitHubGateway` whose reads ride
  the hermetic checkout while the GitHub API serves the hosted surface.
  Each round posts one review (event pairing: blocking →
  REQUEST_CHANGES, clean → APPROVE, with inline comments anchored by
  `line` + `side` on confirmed findings' first cite), replies
  re-raises into identity-matched threads naming the reviewed commit,
  resolves threads whose issues retired (two clean misses) or were
  rejected by the owner (each with a note reply), and upserts one
  standing verdict comment — the rendered verdict plus the cross-round
  state marker, edited in place with three bounded retries. Stage-0
  evidence comes from check runs: annotations on both ends of the
  pinned range map to tool findings (failure→critical,
  warning→warning, notice→suggestion) and ride the existing
  differential attribution. `--actor` gates the run on collaborator
  write access before any spend; `--dry-run` previews the comment and
  posting plan with zero writes; a checkout that is not the PR head
  fails closed naming both shas. Pinned by the hosted suite (posting
  plan matrix incl. solo-never-threads and resolution-waits-for-
  retirement), the wire pins (GraphQL documents, viewer login,
  `APPROVE` mapping, marker scan), the fake-gateway e2e (posts,
  resolves, marker persistence, refused actor, dry run), and the
  binary e2e (missing `GITHUB_TOKEN` exits 2 naming it); live
  read-only GETs sit behind `DIFFTRACE_E2E=1` + `GITHUB_TOKEN` +
  `DIFFTRACE_TEST_REPO`/`DIFFTRACE_TEST_PR` in `tests/github_live.rs`,
  skipping silently offline.

- **Suppressions CLI** — `difftrace reject <finding-id> [--reason]
  [--theme]` and `difftrace suppress list|clear [id]`: the owner's kill
  switch over the persisted round state. `reject` resolves the id in
  the checkout's newest report (confirmed, solo, and unverified
  tiers), builds the suppression from the finding itself — theme = its
  normalized title tokens, glob = its evidence file, widened by
  `--theme` to the directory glob — flips every same-identity standing
  issue to `Rejected` naming the new id, and prints a receipt; `list`
  renders hit counters; `clear [id]` removes one or all and reopens
  exactly the issues the removed suppressions had rejected, so no
  dangling rejection survives into the next round. State-only commands
  pin at head through the new `LocalGateway::open_head` (no base
  coupling, no diff fetch). Pinned end to end against a scratch repo
  (reject-by-id, unknown-id error naming the report it searched,
  list-hits-and-clear, clear-all-reopens, plus state-level theme/glob
  and standing-set pins).


- **Chat** — `difftrace reply <question> [--mode answer|plan]`: one
  stateless answer call over the persisted surfaces — the newest
  report's findings (tier, id, severity, title, mechanism, cites), the
  standing open issues, and the owner suppressions. Chat is
  structurally incapable of touching findings (its only output type
  is text, pinned by a constructor witness), renders inert through the
  same escape rule as verdicts, and a `reject <finding-id>` intent
  answers with the exact `difftrace reject` command to run — the
  confirmation is running it; the reply path never writes state
  (pinned byte-identical). Thread and PR targets arrive with the
  hosted gateway; locally the target argument is the question.


- **Panel serialization pins** — a counting client proves two judges
  on one provider serialize through the provider lane (peak one model
  call in flight with `per_provider_lanes = 1`, both sweeps still
  delivered), and two-of-three confirmations block at the fold. The
  panel machinery itself shipped with the judge pass; these pins lock
  its guarantees in place.


- Per-run durations in the judge logs. Each engine run inside a region
  attempt — the primary verification pass and, when the report needs
  repair, the corrective turn — logs a `run finished` line with its own
  `dur`, `turns`, and run kind (`primary`/`corrective`); the region
  lines gain `run_secs` (the full per-run list for every attempt), and
  `RegionStats` carries `run_secs` onward into the persisted verdict,
  so a slow run is attributable to the sweep, the region, or the
  individual model call.


- `ratelimit.global_lanes` (default 8): the outer concurrency pool
  every per-provider lane nests inside, now configurable instead of
  hard-coded. A single-provider panel that wants more than 8 concurrent
  requests sets it alongside `per_provider_lanes` — 20 lanes on one
  host now actually means 20 in flight. Pinned from both sides: a
  raised pool admits that many acquires at once, and a smaller global
  still binds across hosts.


- Per-profile model tuning under `[[judges.model]]`: each entry names a
  profile string and overrides three knobs for it alone — `effort` (the
  full reasoning ladder: none, minimal, low, medium, high, xhigh, max),
  `replay_reasoning` (replay prior-turn thinking so multi-turn judges
  keep their own reasoning in view), and `max_tokens` (a per-profile
  ceiling). Profiles without an entry inherit the global `judges`
  values; unknown effort spellings and nameless entries reject at load.
  The zai and anthropic arms ride loopctl's native thinking-effort,
  reasoning-replay, and output-ceiling builders; the local arm rides
  the self-hosted builder's effort and replay — `local` profiles do
  not take `max_tokens` (the self-hosted endpoint owns its output
  ceiling) and reject the knob loudly when it is set.

### Fixed

- **A cite into a window the judge's own tool served is grounded** — the
  45dee27 run judged 30 defect cells and surfaced zero findings: 46 of 56
  weak leads died on cite shape because judges legitimately cite head-file
  lines their `read_file_at_head` views showed them, lines neither the
  diff hunks nor the 400-line snapshot window admit. The read tool now
  records every window it actually serves (`ServedReads`, threaded
  run→sweep→region→registry, the span parsed from the numbered view so a
  byte-cut page records only the lines it truly carried), and
  `CiteAuthority` admits any served line in any scope — evidence and
  mechanism alike (a mechanism walking to a served line of the region's
  file passes). Pinned by `a_served_read_records_the_window_the_judge_saw`,
  `the_served_window_parser_reads_only_numbered_lines`,
  `a_tool_served_line_admits_a_cite_any_region_kind`,
  `a_mechanism_citing_a_served_line_of_the_region_file_passes`, and
  `a_finding_citing_a_tool_served_line_becomes_a_solo_finding`.
- **Transient provider errors die in the engine, and a quota wall cools
  for real** — judge loops now install loopctl's `StreamHandler`
  (`BareLoop::new_with_managers`), so transport blips and rate-limit
  rejections retry in-flight with the provider's own Retry-After instead
  of surfacing to the region ladder (the v1 hardening restored; the
  region ladder stays the outer net for sustained outages). Header-less
  rate-limit trips escalate with the consecutive streak —
  base·2^(streak−1), capped at the cooldown cap — instead of a 0–5s
  first-attempt draw, so a tokens-per-minute window can actually pass; a
  hinted trip or a lapsed deadline resets the streak. A panel whose
  judges all ride one provider warns before any spend. Pinned by
  `a_transient_stream_error_is_absorbed_by_the_engine_ladder`,
  `header_less_trips_escalate_with_the_streak`,
  `a_streak_resets_once_the_deadline_lapses`, the reworked ladder pins
  (stall/no-retry, half-span requeue both ways, failed-run seconds) on a
  retryable-error stream client, and `a_one_provider_panel_warns_before_spend`.
- **Same-title restatements collapse** — the weak-lead collapse keyed on
  mechanism-token overlap alone never fired on the 45dee27 corpus (0 of
  56 collapsed): real restatements share a title and word their
  mechanisms differently. The key is now claim + aspect + (mechanism OR
  title overlap), pinned both ways by
  `restatements_sharing_a_title_collapse_without_mechanism_overlap`.
- **Region wall time counts in-lane time** — `RegionStats.secs` started
  at task spawn, before the lane permit, so queued regions inflated the
  run's summed wall (107 h against a 1 h 45 m run). The clock now starts
  at lane acquisition; pre-lane paths (budget-exhausted, lane failure)
  record zero.
- **Hosted inline comments anchor only inside the PR diff** — under
  `--scope deep` (or `review.scope = "deep"`), judges legitimately cite
  head-file lines outside the diff hunks, and a confirmed finding whose
  first cite is snapshot-only minted an inline review comment at a line
  the PR diff does not contain — GitHub rejects the one atomic
  submission (422) and the whole hosted run exits 2 after full model
  spend. `plan_posting` now takes the pinned `DiffIndex` and mints an
  inline comment only when `index.resolve` admits the cite; a
  snapshot-only cite rides the standing verdict comment, and a
  thread-matched re-raise still replies (replies anchor to threads, not
  diff lines). Pinned by `a_snapshot_only_cite_never_mints_an_inline_comment`,
  `a_diff_pinned_cite_mints_the_inline_comment`, and
  `a_snapshot_only_cite_still_replies_into_a_matched_thread`.
- **The judge wire offers only the schema's claim set** — the parser
  accepted `"adjacent-preexisting"`, a tag `REPORT_SCHEMA` never offers
  and only reconcile mints (the relabel from `fa7e883`). The parse arm
  is gone: a judge emitting the tag fails its region as malformed
  naming the claim, exactly like any other schema-violating spelling,
  so wire and schema cannot drift. Pinned by
  `a_schema_violating_adjacent_preexisting_claim_rejects`.
- **An unpaired `--- ` line before a new file never leaks into the
  previous file's window** — in a mangled diff whose `--- a/g` is
  followed by `diff --git` rather than `+++`, `region_diff` kept the
  line as a removed line of the previous file while the index consumed
  it as metadata. The window now treats `--- ` at exhausted counts as a
  file boundary whenever the lookahead starts a new file (`+++ `,
  `diff `, `index `, or a hunk header), keeping the renderer and the
  index in agreement. Pinned by
  `an_unpaired_dashes_line_before_a_new_file_never_leaks_into_the_window`.
- **The escalation invariant comment sits beside its function** — the
  one indented body comment left in non-test code (canon §2) is hoisted
  to an item-adjacent comment above `escalation_job`, matching
  `tools_deadline`'s shape; a crate-wide sweep pins zero indented `//`
  comments outside test modules.
- **Same-title restatements really collapse** — the key claimed above
  shipped with only the mechanism arm: `titles_overlap_enough` was never
  called on titles, and the pin's two entries shared one fixed mechanism
  string, so it collapsed by mechanism identity and exercised nothing. The
  title-overlap disjunct is implemented, and the fixture's two mechanisms
  now word differently (hand-checked: zero shared tokens, so only the
  title arm can fire). Pinned for real by
  `restatements_sharing_a_title_collapse_without_mechanism_overlap`.
- **Served windows include line one** — `served_window` skipped the first
  line of the inner read output assuming a header, but loopctl's offset-1
  views carry no header: the first line is numbered line one itself, so
  every default read recorded `(2, N)` and a cite to line 1 of a served
  file weakened as cite-outside-materials. The skip is gone; the fixtures
  mirror the real whole-file and offset-header shapes and pin `lo == 1`
  (`the_served_window_parser_reads_only_numbered_lines`,
  `a_served_read_records_the_window_the_judge_saw`).
- **Exec-settled lead verdicts survive the lead oracle** — the lead-oracle
  pass replaced the whole `lead_verdicts` map, silently dropping cheap
  `git check-ignore` / `cargo package --list` verdicts recorded for
  non-NeedsExecution leads whenever any needs-execution lead existed. The
  pass now merges per finding id, with the oracle result winning where
  both exist. Pinned by
  `exec_claim_verdicts_survive_beside_lead_oracle_results`.
- **The snapshot byte budget binds under the line cap** —
  `snapshot_window` seeded its head window above short files' totals, so
  the shrink loop read zero head bytes and settled with head ≥ total:
  files of at most 200 lines over 32 KiB shipped whole. The head clamps
  to the total before the shrink. Pinned by
  `a_short_fat_file_shrinks_by_bytes_below_the_line_cap`.
- **Region materials are hard-capped at the byte budget** — a single
  unsplittable line (or a depth-five split remainder) rode the judge
  prompt whole past `REGION_MATERIAL_BUDGET_BYTES`; `region_prompt` now
  cuts the material at the budget with a marker naming the cut. Pinned by
  `oversized_region_material_ships_capped_with_a_marker`.
- **Test-module demotion tracks literal-stripped braces** — an unbalanced
  brace inside a string literal held the `#[cfg(test)]` span open past
  its close and demoted later production lints to warning; the span now
  counts braces over `flow`'s comment-and-string-stripped lines (the
  stripper already in tree). Pinned by
  `braces_inside_string_literals_do_not_drift_the_module_span`.
- **The comment sweep pin exists** —
  `no_indented_comments_outside_test_modules` in `src/lib.rs` walks the
  crate beside the header and per-item-doc sweeps, tracking test-module
  spans over stripped lines, making the earlier claim about zero indented
  `//` comments outside test modules an enforced property.
- **`in-diff` means the diff** — pre-existing defects (install snippets
  pinning a version two minors behind, present before this branch) rode
  as `in-diff` whenever their cite resolved anywhere in the diff's
  context lines. A valid `in-diff` finding whose evidence cites no line
  the diff actually ADDS now relabels to `adjacent-preexisting` (new
  claim tag, rendered "adjacent-preexisting (pre-existing)"): the
  finding still surfaces, never blocks the round, and release-blocking
  triage can finally split branch-caused from adjacent debt. Pinned by
  `a_pre_existing_snippet_cited_on_an_unchanged_line_is_adjacent` (both
  directions: the unchanged-line snippet relabels, the added-line
  mechanism keeps `in-diff`).
- **Cheap claims are executed, not argued** — the loopctl run recorded
  `tool_calls: 0` while judges statically asserted facts a single command
  would settle (`git check-ignore` on AGENTS.md, `cargo package --list`
  on the Makefile gate). The new `execclaims` pass derives a whitelisted
  command from a lead's own mechanism text (`git check-ignore -q --` for
  untracked/unignored claims, `cargo package --list --allow-dirty
  --no-verify` for packaging-gate claims), runs it once through the
  process executor under the oracle's budget and per-run cap, and records
  `Confirmed`/`Refuted{command}` into the finding's lead verdict —
  mechanisms with no command shape record nothing rather than guessing.
  No judge calls; results never insert over an oracle verdict. Pinned by
  `an_unignored_claim_is_refuted_when_check_ignore_matches`,
  `a_package_list_failure_confirms_the_gate_defect`,
  `a_mechanism_without_a_command_stays_unverifiable_by_execution`, and
  `the_per_run_cap_bounds_claims`.
- **A mechanism restated per region collapses, and judge families are on
  the record** — 47 of the loopctl run's weak+dropped findings were one
  claim ("the new metric emissions have a presence gate and no behavioral
  pin") restated once per region. Weak leads sharing a claim, aspect, and
  ≥50% mechanism-token overlap now collapse into one lead whose
  `also_at` site list names every affected `file:line` (rendered "— also
  at …"); the coverage defect accounting keys on the pre-collapse
  (judge, id) set so collapsing never flips a cell to defect. Beside it,
  `Reconciled.families` records the distinct model families behind the
  votes (`judge_family`: `zai:glm-5.3` and `zai:glm-5.3-flash` are both
  family `glm-5.3`; replica suffixes strip), and the confirmed badge
  shows `(1 families)` when two votes ride one family — two-of-N
  agreement is no longer readable as independent confirmation. Pinned by
  `a_mechanism_restated_per_region_collapses_into_one_lead_with_sites`
  and `a_two_vote_same_family_confirmation_records_one_family`.
- **doc-refs reads link targets and respects gitignore** — all six of the
  loopctl run's doc-refs findings were false positives: backticked prose
  (README's conceptual `loop.yaml`, the vendored `curated_prices.json`),
  paths inside quoted error text (`/tmp/probe.py`), and a deliberately
  gitignored target (`target/provider-conformance/matrix.md`, confirmed
  by `git check-ignore`). Only markdown link targets (`](path)`) count
  as references now — backticks are prose; anchors, URLs, mailto, and
  absolute paths are not repo paths — and before claiming a dangle the
  check asks the gateway's new `check_ignore` (local gateway:
  `git check-ignore -q`; default false), so an unpacked-on-purpose path
  stays quiet. Pinned by the reworked docrefs suite
  (`backticked_prose_is_never_a_reference`,
  `an_ignored_link_target_is_deliberately_unpacked`,
  `absolute_paths_and_anchors_are_not_repo_paths`,
  `a_dangling_link_target_is_a_finding`).
- **Lint severity knows its module, and unenforced lints are the finding** —
  the loopctl run shipped 20 critical clippy hits inside
  `crates/derive/src/attr.rs`'s `#[cfg(test)]` module (the deny-level
  `unwrap`/`expect` family this house allows in test code) while the one
  true defect read as a lesser warning. Tool findings whose file:line
  sits inside a `#[cfg(test)]` module now demote critical → warning with
  an explicit `[test-module code]` note naming the crate's own test-lint
  policy (brace-tracked module spans from the head snapshot; an
  unreadable file keeps the original severity). Beside it, the new
  deterministic `ci-coverage` stage-0 check names the structural fact:
  a crate the diff touches that no CI leg compiles with a lint gate is
  one warning finding — verified against the loopctl corpus that
  `loopctl-derive` IS covered by the workspace-wide clippy leg (no
  finding fires there). Pinned by the testcontext suite
  (`a_lint_inside_the_cfg_test_module_is_demoted`,
  `the_test_module_closes_and_later_production_code_recovers`) and the
  cicoverage suite (`a_workspace_wide_clippy_leg_covers_every_member_crate`,
  `a_crate_no_leg_compiles_with_a_gate_is_the_finding`,
  `an_untouched_crate_never_fires_even_without_any_gate`).
- **A broken citation is repaired from the mechanism, not deleted** — all
  13 weak leads of the loopctl run died on cite shape (2 unparseable
  evidence, 5 mechanism-out-of-region, 6 cite-outside-materials) while
  their mechanisms named exact `file:line` sites the whole pinned diff
  admits. Weakened findings now get one bounded repair pass: cites mined
  from the mechanism text (`mine_cites`) are re-anchored against the
  full diff index — not just the region span — and the finding keeps its
  demotion with a resolved cite instead of empty evidence. Nothing runs
  a model; the pass is pure and once per finding. Pinned by
  `a_broken_cite_is_repaired_from_the_mechanism_not_emptied` (all three
  fixture shapes replayed).
- **A malformed title is repaired from the mechanism, never destroyed** —
  33 of the loopctl run's 34 dropped findings died on title shape alone
  (4 of them warning-grade), taking their mechanisms, severity, and
  evidence with them. A title that is empty or over 80 characters is now
  derived from the mechanism's first clause (`repaired_title`, split on
  sentence boundaries, capped with an ellipsis) and the finding survives
  as a weak lead carrying the `repaired` original title on record plus a
  `[title repaired]` marker in the render; only a finding with nothing to
  derive a title from still drops. Pinned by
  `a_malformed_title_is_repaired_from_the_mechanism_not_dropped` and the
  reworked fabrication-guard pins in
  `a_structural_failure_still_drops_not_weakens`.
- **Unjudged regions are visible and a region a compactor cannot shrink
  re-queues smaller** — coverage now distinguishes "a cell exists" from
  "every present judge actually judged it": a judge whose region failed
  leaves the cell counted `short` in the per-file audit trail and named
  in a `⚠ N cells short of the full panel across M files` line, instead
  of the half-judged file reading as fully clean (the loopctl run judged
  `338/338` while both judges were incomplete). Region sizing is now
  byte-aware — a file of few but very long lines (CHANGELOG.md: 138
  lines, 122KB of diff) split under the new 96KiB materials budget
  `REGION_MATERIAL_BUDGET_BYTES` (`fit_regions`), the snapshot window
  shrinks by bytes as well as lines, and a compaction stall on a
  splittable span drops its lane permit and re-queues exactly two
  half-span attempts once, merging their verdicts (defect beats clean)
  and failing loudly with `a half-span re-queue still failed` when both
  halves still stall. Pinned by
  `a_failed_region_shortens_coverage_and_names_its_files`,
  `a_file_of_very_long_lines_splits_by_bytes_not_just_lines`,
  `a_snapshot_of_very_long_lines_shrinks_by_bytes`,
  `a_compaction_stall_requeues_two_half_span_attempts_once`, and
  `a_stall_that_survives_the_half_span_requeue_fails_loudly`.
- **Judge conversations can no longer be dominated by one read or one
  region** — the judge read tool is now loopctl's paged `ReadTool` over
  a `ContentSource` that reads through the pinned-head gateway
  (`read_file_at_head` keeps its name; the schema gains
  `offset`/`limit`/`line_range`, one view carries at most 400 numbered
  lines / 32 KiB, and a cut view always names where reading continues),
  so no single tool result can exceed the context window the way a
  whole-file read of an 11k-line file once did — the dch precedent,
  acquired from the engine rather than reimplemented. The per-region
  budget and the unchanged-notice stay as wrappers (repeat detection
  keys on the exact window, so paging never trips the notice), gateway
  failures surface as tool errors naming the path, and the pinned git
  reads ride `spawn_blocking` off the async workers. Pinned by the
  judge-tool suite (`a_default_read_pages_large_files_instead_of_shipping_them_whole`,
  `an_offset_read_continues_where_the_default_window_stopped`,
  `repeat_detection_keys_on_the_window_not_the_path`,
  `a_failed_pinned_read_surfaces_as_a_tool_error`,
  `the_read_schema_advertises_the_paging_fields_under_the_pinned_name`,
  and the reworked notice/budget pins).
- **A compaction stall is no longer retried as if it were transient** —
  loopctl's `Compaction made no progress … cannot shrink this
  conversation further` is deterministic for the same conversation, so
  it now classifies like the stream budget (one attempt, no retries,
  the cause naming the stall) instead of burning the retry ladder four
  times over fifty-five minutes on the same region. Pinned by
  `a_compaction_stall_spends_no_retry` and the classify pin in
  `failure_kinds_classify_their_causes`.
- **The context window is a knob, not an inherited 200k assumption** —
  `judges.context_window` (default 200000) anchors loopctl's
  compaction thresholds on judge sessions, overridable per profile
  under `[[judges.model]]`; zero rejects at load on both levels, since
  a zero window silently disables the compaction guard. Pinned by
  `the_context_window_resolves_per_profile_and_rejects_zero` and
  `the_session_carries_the_configured_context_window`.
- **Full-scope regions of changed files split by their real span** —
  the diff index now bounds each file's repo region at its last
  anchored diff line, so a changed file's whole diff — the shape that
  once put 148,899 tokens into one opening prompt — splits under
  `region_max_lines` like any other region, and cites outside a chunk
  weaken instead of validating; files the diff never touches stay one
  region. Pinned by
  `repo_regions_take_real_ends_from_the_diff_and_leave_untouched_files_whole`
  and `bounded_repo_regions_split_like_any_other_span`.
- **Failed judge runs still count their wall seconds** — a primary or
  corrective run that errors no longer vanishes from `RegionStats`
  with `turns=0`; its seconds ride `run_secs` and the region line
  stays attributable. Pinned by
  `a_failed_attempt_still_records_its_run_seconds`.
- **A partial hosted round never posts approval** — when judges are
  absent or regions fail, the posted review carries the neutral
  `COMMENT` event instead of `APPROVE` (or `REQUEST_CHANGES`): an
  incomplete panel can no longer satisfy a required-review count for a
  PR it did not finish reviewing, restoring the v1 guarantee the
  rewrite had regressed. Pinned by
  `a_partial_round_withholds_both_approval_and_rejection` and the
  hosted e2e (`review:COMMENT` on a partial panel).
- **`local` judge profiles reject `max_tokens` loudly** — the knob was
  silently ignored for self-hosted endpoints (the builder has no
  output-ceiling setting and the endpoint owns its own); a non-default
  `judges.max_tokens` or `[[judges.model]]` override for a `local`
  profile now errors naming the knob, its value, and the remedy,
  instead of pretending to apply. Pinned by
  `a_local_profile_rejects_a_non_default_max_tokens_loudly`.
- **Same-second report writes no longer invert report selection** —
  `reject`/`reply` pick the newest report by parsed `(timestamp,
  collision-suffix)` order, so a second write within the same second
  (`.1` suffix) wins over the unsuffixed name it follows, where the
  byte-wise sort read the older file. Pinned by
  `a_same_second_collision_still_picks_the_last_written`.
- **The reusable workflow drops the dead `model` input** — the rewrite resolves
  models via profile `:model` parts and provider env vars; the v1-era
  `DIFFTRACE_MODEL` pass-through had no reader, so a caller setting it
  silently reviewed with the panel default. The input, the env
  forward, and the caller-template pass-through are gone, pinned by
  the workflow contract suite (no `model:` input, no `DIFFTRACE_MODEL`
  anywhere).
- **The newest report is really the newest** — `reject` and `reply`
  sorted the checkout's report files before picking (directory order
  is arbitrary), so both could read a stale round and reject/reply
  keyed on the wrong cites. Pinned by `a_stale_report_is_never_the_newest`
  (an id that exists only in the older report errors against the
  newest) and the reject fixture now always carries an older sibling.
- **The oracle classifies only meaningful cargo exits** — exit 0
  refutes (under the test policy only when the run executed at least
  one test — a filter that matched nothing runs zero tests and
  proves neither side, and a run with no `test result:` line at all
  proves nothing either); only exit 101 — a genuinely failing test
  under the test policy, a compile failure under the check policy —
  confirms; everything else, including cargo's exit-1 CLI-usage
  errors, stays inconclusive and can no longer promote a solo finding
  or silently refute a real one. Dash-leading tokens never become
  flag arguments (a lone dash is cleaning noise; a dash-leading path
  stem is skipped). Pinned by the exit-matrix suite plus a
  real-toolchain integration test driving a genuinely failing test
  through the process executor (`a_real_failing_test_confirms_through_the_process_executor`)
  — the pin that holds the map to measured cargo reality.
- **Check runs and annotations no longer truncate at the first page**
  — both GitHub endpoints page to their real maxima (100/50 per page),
  so a matrix CI beyond the first page still reaches stage-0 evidence
  and the differential attribution compares full sets. Pinned by the
  route/page suite.
- **Colon-less line attempts count as discarded evidence** — a
  digit-bearing string without a colon (`line 42`) is a failed cite
  attempt and lands the finding in the unparseable bucket, not the
  outside-materials one; prose without digits still never counts
  (the earlier decision stands). Pinned by
  `a_colon_less_line_attempt_counts_as_discarded`.
- **Quoted env-file values lose one matching layer** —
  `ZAI_API_KEY="sk-…"` in `~/.difftrace/env` no longer stores the
  quotes as part of the key; unmatched quotes stay literal and only
  one layer strips. Pinned by
  `quoted_env_file_values_lose_one_matching_layer`.
- **The hosted fork guard covers comment events** — the guard runs for
  every trigger after the token mint and before any checkout:
  `pull_request` events read the payload flag, comment events resolve
  the pull request and compare head owner to base owner, so a `/review`
  from a fork contributor can never make the app token read external
  code. The caller template's false "issue_comment events only fire
  for base-repository comments" rationale is corrected. Pinned by the
  workflow ordering suite (mint → guard → checkout, no
  event-scoped guard).
- **Help and version print as help, not errors** — `--help`, a
  subcommand's `--help`, and `--version` exit 0 with their text on
  stdout (display-class clap errors route through clap's own exit);
  the `usage error:` label and exit 2 belong to real parse failures
  only. Pinned by `help_and_version_print_cleanly_and_exit_zero`
  (binary e2e), which also pins the kept label on a bad flag.

### Changed

- The internal rewrite codename is gone from every shipped file: the
  eval parity flag and runner value are now `--current` / `current`
  (`make eval-run-current`), the parity report renders
  `current-vs-v1.md`, and the public finding type is `Finding`
  (renamed from its codename-bearing spelling; serialization is
  field-name-based and unchanged). Third-party action version tags
  and the semver spec URL are version pins, not the codename, and
  stay.
- Findings failing only the evidence-citation rules (cite outside the
  region materials, mechanism citing no region line, unparseable
  evidence) no longer vanish into the dropped pile — they land in a
  visible **weak leads** tier: rendered in the verdict under
  "Unverified leads", counted in the footer, excluded from the defect
  coverage and the blocking count, and tried by the oracle. Structural
  failures (malformed title, uncapped assumption, over-ceiling
  severity) still drop. The round that motivated this raised 104
  findings and discarded every one for citation shape — the substance
  survived, the wrapper did not.
- `judges.region_max_lines` default raised 400 → 3000: whole changed
  files now ride one judge conversation where they fit, collapsing the
  per-region conversation fan-out that made a large review take hours
  (640 engine runs for one diff) — most reviews land at one
  conversation per file per judge.


- Judge region tools (`read_file_at_head`, `search_at_head`) can no
  longer produce the byte-identical answers loopctl's loop detector
  folds into a repetition triple. The detector reads the generic tool
  signature, so its primary param is empty and the triple is effectively
  (tool, result-text): three identical answers — a judge re-reading one
  file, budget refusals for several files, or bare `no matches` for
  several patterns — read as one loop. Three shapes close it: a repeat
  identical call answers with an explicit unchanged notice (checked
  before the budget, so repeats also stop consuming it), every budget
  refusal names the target it refused, and every empty search names its
  pattern. No two answers from one tool share text unless the calls and
  their results genuinely are identical.

- loopctl re-resolved from the 0.3 line (`#5785d24`) to current master
  (`#d365f3e`, 0.4.1). The 0.3 engine dropped judge reasoning across
  mid-run compaction, so judges re-read and re-asked the same regions
  until the loop detector fired on every turn — the review-run death
  spiral. The 0.4 line carries the retention set (turn-scoped reasoning
  retention, the compaction progress ledger, compaction durability,
  typed retention). A dependency-floor test
  (`tests/dependency_floor.rs`) holds the line: the resolved loopctl
  must be at least 0.4.0, so a future `cargo update` cannot silently
  reintroduce the pathology.

### Added

- The run orchestrator (`src/orchestrator.rs`): the single
  driver that carries a review from the deterministic tool layer to
  the persisted round state. `RunInputs` bundles the pinned gateway,
  the judge panel, the coverage plan, the `RunBudget` ceilings
  (total, tools, oracle), and the state/cache directories;
  `run_review` drives production judge clients, `run_review_with`
  is the shared-client seam end-to-end tests drive. Exit codes
  follow the contract: 0 clean full round, 1 confirmed blocking
  findings, 2 infrastructure failure (nothing posted, state
  untouched), 3 partial — the round is not full (uncovered cells,
  absent judges, or a judge with failed regions), the same predicate
  that freezes issue misses. On
  exit 3 the only state that moves is suppression hit counters
  (`state::apply_partial_round`, pinned) and the returned verdict
  carries that persisted state, so the rendered and JSON state lines
  never show a round the disk does not hold (pinned); full rounds
  persist the post-round state atomically. Judge sweeps that cannot
  start
  inside the remaining budget are marked absent, and a wall budget is
  opt-in: the default `--budget-secs` is zero — no ceiling, the run
  finishes its concurrent work and ends when the panel is done —
  while an explicit `--budget-secs N` (or `DIFFTRACE_BUDGET_SECS`)
  bounds the whole run as before: the sweep stops at the deadline
  with the remaining regions recorded as failed (one WARN, no calls
  past it), each region's stream timeout caps at the remaining time,
  and the oracle's budget caps at the remainder (pinned); tool
  unavailability is logged and never fatal, and the idempotency
  property — two runs on identical inputs yield equivalent verdicts
  — runs in CI (pinned). `RunOutcome` moves to `contracts.rs`
  unchanged; `cli` re-exports it.
- Local review wiring: `difftrace review` (no `--repo`) now runs the
  full pipeline against the local checkout and renders the console
  verdict or, with `--json`, the machine-readable verdict;
  `--fix-prompts` reaches the renderer; a run transcript lands under
  `~/.difftrace/reviews` per run; `RUST_LOG` drives diagnostics from
  the binary's entry point (pinned end-to-end by a binary test in a
  fixture checkout: exit 3 without provider credentials, transcript
  written, state file at round 0). `--dry-run` keeps the cheap
  plan-and-estimate preview and gains a read-only state-delta line
  (round N→N+1 preview, standing issues, active suppressions; "no
  previous round" on a fresh checkout — pinned).
- Verdict rendering (`src/render.rs` + `render/console.rs` +
  `render/comment.rs` + `render/json.rs`): the mechanical verdict with
  no model anywhere in rendering. One shared section builder carries
  the layout — Verdict (🟢 good to go / 🔴 N confirmed blocking
  findings, the solo/tool sub-line, and ⚠ partial-round lines when
  judges are absent or incomplete), Coverage (regions, cells judged/total by N
  judges, clean/defect, and the per-file table), Confirmed findings
  (`N. [severity|aspect|votes/panel] Title — file:line`, indented
  mechanism, claim labels with assumptions marked), Tool findings,
  Solo (unconfirmed — never blocking), the
  suppressed/refuted/dropped footer, and the State line (round,
  standing, retired this round, suppressions). The console sink
  prints the sections; the hosted comment sink appends a `<sub>` run
  range for the future hosted gateway; the JSON sink pretty-dumps the
  whole `RoundVerdict` and round-trips it (pinned). The optional
  `RenderOptions { fix_prompts }` adds one 4-space-indented block per
  confirmed finding — mechanism plus `Fix:` hint — absent by default
  and never for solo or refuted findings (pinned). Every
  model-controlled inline field (titles, fix hints, tool messages,
  mechanisms) is markdown-escaped — inside a list item a four-space
  indent is a lazy continuation, not a code block, so a mechanism in
  the findings list escapes like every other inline field — and the
  fix-prompt block renders mechanisms as indented code after its
  heading, so nothing the judges or tools write can render a link, a
  heading, or close a fence inside the comment — the same bytes ship
  to the console and the hosted surface (pinned with disguised links
  and crafted paths, link- and image-shaped mechanisms, and
  heading-shaped lines). `CoverageSummary` gains
  serde-defaulted `regions` and `per_file` (`FileCoverage { cells,
  judged, clean, defect }`), filled by the reconcile fold under the
  same threshold and validated-defect rules (pinned per file:
  a judged defect cell and a minority-uncovered cell land in their
  own files) — the coverage matrix is now visible per file.

- Local run transcripts and diagnostics (`src/logging.rs`): the
  RUST_LOG-filtered subscriber (fmt + env filter + span-close events
  on stderr; debug and trace directives quiet the HTTP stack, empty
  falls back to info — pinned) and the transcript writer —
  `<checkout>-<ts>-<sha>.log` under an injected directory, a versioned
  first line, the rendered body, and checkout labels sanitized with
  the state-file discipline so a hostile label cannot escape the
  directory (pinned). Two writes for the same checkout and sha inside
  one second land in distinct, existence-suffixed files — an audit
  record never overwrites its neighbor (pinned). A second artifact
  lands beside every transcript: `<checkout>-<ts>-<sha>.report.json`,
  the full `RoundVerdict` in machine-readable form (including the
  per-region stats below), and the transcript itself gains a
  `## Region stats` section — one line per settled region (judge,
  region, seconds, turns, tool calls, input/output tokens, outcome)
  plus totals. `tracing` and
  `tracing-subscriber` (env-filter) join as dependencies. v1's
  run-activity `LoopObserver` mirror is not ported: the bare-loop seam
  hosts no observers yet.

- The lazy oracle (`src/oracle.rs`): mechanical truth-coupling for
  execution claims. `verify(claims, policy, repo, head_sha, budget,
  max_claims, executor, cache_dir)` selects only
  `NeedsExecution` claims — in-diff and assumption findings are never
  verified, so nothing already confirmed by quorum can meet an oracle
  refutation — capped at `max_claims` per round and bounded by one
  wall budget checked before every attempt: a claim runs only while
  at least a second of budget remains and its command timeout is that
  remaining budget, so a sub-second remainder is a spent budget (the
  claim stays absent from the results, unattempted, while an
  attempted but undecidable claim records `Inconclusive`: reconcile
  treats both as
  solo, and the recorded verdict is auditable and cached). A claim
  must first assert failure (markers over the lowercased title and
  mechanism: fail, panic, does not compile, won't compile, crash,
  hang, deadlock); only then does a command run — `cargo check -p
  <crate>` under the check policy, `cargo test -p <crate>` under the
  test policy with the target extracted from the claim text (the
  first token containing `test_`, ending `_test`, or containing
  `::test`; else the stem of the first `.rs` path token, riding as
  cargo's name filter; else package-wide). The crate comes from the
  workspace map over the claim's first evidence file — a claim no
  crate owns is inconclusive without a run. A succeeding command
  refutes the failure claim and carries the exact command; a failing
  one confirms it; timeouts, launch failures, and keyword-less
  claims stay inconclusive. Verdicts cache per (head sha, policy,
  claim id) — a policy flip or a new head re-verifies, and a
  budget-starved round's inconclusive verdicts do not outlive their
  sha (pinned by the oracle suite: refutation carrying the command,
  inconclusive solo paths for timeout and keyword-less claims without
  a run, budget exhaustion leaving later claims unverified with the
  sub-second boundary pinned at both edges, the per-round claim cap,
  selection
  skipping non-execution claims, off policy running nothing, check
  and test command targeting incl. the unowned file, cache behavior
  across shas and verdict kinds, failing runs confirming).

- Reconciliation (`src/reconcile.rs`): the pure fold from judge
  reports to a `RoundVerdict`.
  `reconcile(reports, plan, state, oracle, aspects, index, panel,
  tools)` validates every Defect through `validate_finding` (invalid
  findings land in the verdict's `dropped` list with their reason and
  never vote), groups the rest by issue identity (same file plus
  `same_spot`: an equal line with shared wording, or ±5 lines with
  half the wording), votes once per distinct judge, and assigns status
  in precedence order — a matching suppression outranks everything;
  two votes, or one vote plus an oracle confirmation, confirm; the
  oracle never demotes a two-vote confirmation but refutes a solo;
  everything else stays solo and never blocks. Severity is the vote
  median (an even split takes the lower middle — a warning+critical
  tie reconciles to warning), capped by the aspect ceiling and the
  assumption claim cap (ExternalAssumption ≤ suggestion). Coverage
  counts a cell as covered when `max(panel − 1, 1)` distinct judges
  hold a verdict (out-of-scope counts, with an empty result), and
  `cells_defect` counts a covered cell as defect only when a
  surviving judgment is a Defect. The verdict carries the absent
  judges (panel minus present), and its `state` field is the
  post-`apply_round` state — the fold runs the state update itself
  with `covered_gaps` = full coverage and no absences. Contracts gain
  `OracleVerdict`/`OracleResults` (the oracle stage's `verify` returns
  them), serde on `DropReason`, the serde-defaulted `dropped` field
  (order-insensitive under `equivalent()`, pinned with the reordering
  pair, and round-tripped with the `DropReason` and `OracleVerdict`
  wire spellings pinned), and `blocking_count()` (confirmed ≥ warning
  plus tool findings ≥ warning); the clippy
  argument threshold moves to 8 for the fold's flat signature. The
  oracle consult reads every member of a group — any confirmation
  promotes a sub-quorum group, else any refutation refutes it — and
  the severity caps bind to the group's deterministic lowest-id
  member, so a pure reordering of the reports cannot change a
  verdict (pinned by the reconcile suite:
  solo-never-blocks incl. the tool arm, assumption labeling,
  drifted-line confirmation, median with the lower-middle tie,
  merged-group caps with permutation invariance, member-keyed oracle
  promotion and refutation, absent-judge minority coverage,
  suppression removal from voting with the hit counter,
  dropped-not-voted, oracle promotion without demotion, post-round
  state, out-of-scope coverage).

- The judge pass (`src/judges/pass.rs` + `prompt.rs` + `tools.rs` +
  `provider.rs`): one judge's full sweep over the coverage plan.
  `run_judge(spec, cfg, plan, gateway, state, aspects)` builds the
  client from `spec.profile` through the profile factory (zai and
  anthropic via builders with the configured `max_tokens`; local via
  `DIFFTRACE_LOCAL_BASE_URL` + `DIFFTRACE_LOCAL_MODEL`; anything else
  errors naming the profile). A profile may carry a per-judge model
  after the colon — `zai:glm-5.3` overrides the builder's model,
  a `local:<model>` profile satisfies the model requirement the env
  knob otherwise fills, an empty model part rejects, and every
  shipped default panel entry starts (pinned). Bare `zai` and
  `anthropic` profiles take `ZAI_MODEL`/`ANTHROPIC_MODEL` from the
  layered env — the `~/.difftrace/env` file included — with the
  explicit profile model always winning (pinned) — credentials resolve
  through the layered
  env source, process environment first with empty values ignored and
  `~/.difftrace/env` under it, attaching explicitly via the builders'
  `with_api_key` so `ZAI_API_KEY`/`ZHIPUAI_API_KEY`,
  `ANTHROPIC_API_KEY`, and the local knobs honor the env file as the
  README states, a set-but-empty or blank local base URL or model
  errors at start naming the variable, and with no key anywhere the
  builders' own missing-key error survives) and hands it to the generic
  `run_judge_with<C: ApiClient>`, the seam every mock pin rides. Per
  region: the judge system prompt verbatim with the judge name, the
  region's open issues, and its theme suppressions interpolated in a
  single pass (state text containing a placeholder literal cannot
  inject one block into another), the aspect definitions appended;
  region materials — the region diff with ±10 new-side context lines
  (the diff module's free `region_diff` extractor: the window is
  inclusive at both ends and numbers restart from each hunk header,
  and a window spanning hunks opens each further hunk under its own
  `@@ … lines a-b @@` subheader instead of reading as contiguous —
  pinned by the region-window suite in `src/diff.rs`), the file
  snapshot truncated head-and-tail to 400 lines
  (200 + marker + 200), and the flow map for whole-file and repository
  regions; a fresh `BareLoop` per region carrying `read_file_at_head`
  and (when `judges.search_tool` is on) `search_at_head` behind one
  shared per-region call budget — beyond `tool_calls_per_region` the
  tool answers with a refusal naming the budget and the model finishes.
  Regions whose asked aspects were all cut (a `replace = []` path
  override, `--focus`, or a severity floor) are skipped without a
  provider call, and the plan estimate counts `judge_calls` and tokens
  over the regions the pass will call only.
  The report schema rides the prompt (a `response_format` turn
  suppresses tools, and one call per region is the budgeted shape), the
  final turn parses leniently with one corrective turn, and a report is
  well-formed only when it judges exactly the asked aspects —
  anything else, a provider failure, a malformed parse, or the
  `stream_timeout_secs` budget fails that region: it is retried once
  with a fresh agent, and a region failing both attempts is recorded
  in the report's `failed` list (serde-defaulted) while the sweep
  continues — the judge's other regions count, a judge that answers
  nothing is `JudgeAbsent` naming the last cause, and any failed
  region lists the judge in the verdict's `incomplete` set, which
  makes the round partial (pinned). Defects map
  through `FindingDraft`, so ids normalize and evidence cites are
  `file:line`; an evidence string that is not `path:line` is
  discarded rather than fatal — a finding left with no evidence drops
  at the validation gate (`CiteOutsideMaterials`, pinned), so one
  prose citation can never void a judge's whole sweep (pinned) — and
  the schema states the cite shape. `JudgeSpec` carries no sampling knobs: the provider
  request seam exposes no temperature or seed control, so both fields
  and the `judges.temperature` config key are cut rather than silently
  accepted; judge sweeps run at the provider default (the pass-level
  pins are the prompt-verbatim, materials-truncation, state-blocks,
  budget-refusal, search-gating, placeholder-injection,
  well-formed-report, no-partial, zero-cell-skip, and malformed-report
  tests). loopctl joins as the git
  dependency (rev-pinned by `Cargo.lock`, features `zai` + `openai`,
  dev `testing`); `PrGateway` gains `Send + Sync` supertraits for the
  tool boundary. Sweeps are concurrent: the panel's judges run in
  parallel and every region is an independent task through the shared
  `judges.region_concurrency` lane pool (default 8, zero rejects at
  load, pinned), the tool budget stays per region, a failed region
  retries `judges.region_retries` times (default 3, 0 = fail fast,
  10 max — flag `--region-retries` and `DIFFTRACE_REGION_RETRIES`
  override the file) after a full-jitter backoff of up to
  `judges.retry_backoff_secs` seconds (default 8, zero waits
  straight through; the bound escalates 2^n with the retry ordinal;
  `ratelimit::backoff_wait` finally wired, so a
  provider rate limit no longer burns the retry), every settled
  region logs an INFO progress line (`sweep started` with the region
  and lane counts, `region judged` with done/total and elapsed), and
  reports order by region id so completion order cannot change a
  verdict (pinned by the sequential-lane suites). Every settled
  region records `RegionStats` (judge, region, attempts, wall seconds,
  turns, tool calls, input/output tokens from the loopctl run
  record, and — when it failed — the final cause), carried on the
  judge report and the verdict: the
  `region judged`/`region failed` lines carry them live with
  human-readable durations, each sweep ends with a `sweep finished`
  totals line in minutes, and they ride the verdict into the report
  file and stats section (pinned).

- Differential tool attribution (`src/tools/differential.rs`): repo
  debt never blocks a branch — tool findings are measured at both ends
  of the pinned range and only what the branch changed may enter the
  verdict. `attribute` is pure, keying a finding by its exact
  `(tool, file, line, message)` tuple: head-only (or changed) is new,
  present at both shas is context — severity does not re-block, the
  context list is sorted and capped at twenty, and a finding that
  moved lines re-reports as new (the recorded cost of the exact-line
  key; geometric filtering was rejected by design). `run_differential`
  runs one adapter at head and at a materialized base tree through
  the existing run/cache seam (`CacheKey` sha per side, lock digest
  per tree, `CommandSpec.cwd` steering each run); either side
  failing fails the tool closed into the unavailable list — an
  unrunnable base, or one that crashed with no findings on stdout,
  must never launder head findings as new. The head side runs in
  the live checkout, so `run_all` first guards the tree with a
  hermetic `git status --porcelain`: a working tree that differs
  from the head sha fails every adapter closed into the unavailable
  list naming the state — nothing runs and nothing is cached under
  a sha the tree cannot vouch for. A clean tree exports the base
  once (`git archive --format=tar` piped into `tar -x`, the
  repository untouched — no worktree metadata), loops the adapters,
  aggregates new/context/unavailable, and cleans up best-effort.
  `PrGateway::check_runs(sha)` joins the trait default-unsupported
  with a `CheckRun { name, conclusion, summary }` contract for the
  hosted mapping (pinned by the context, new, changed, cap, export,
  fail-closed, crash-base, dirty-head, cross-side, aggregate, and
  hosted-only tests).
- Rate limiting (`src/ratelimit.rs`): the v1 lockstep-storm lesson as
  machinery — `backoff_wait(attempt, base, rng)` is full jitter
  (`rand(0, min(90 s, base·2^attempt))` via a hand-rolled seedable
  `SplitMix64` behind an injectable `Rng` trait, no new RNG
  dependency); `CooldownGate` holds one cooldown per provider host
  (`trip` scales `Retry-After` by `1 + rand·jitter` and clamps the
  result at one hour — an extreme header cannot panic the
  conversion, and a deadline arithmetic that could overflow fails
  closed to a capped deadline instead of dropping the cooldown; a
  header-less trip falls back to one full-jitter first-attempt
  backoff, a re-trip mid-wait only extends, and every waiter on the
  host pauses while other hosts pass untouched); `Lanes` bounds
  concurrency per host (`[ratelimit] per_provider_lanes`) with a
  global `GLOBAL_LANES = 8` cap — permits are acquired host-then-
  global and released on drop: each host's lanes bound it alone,
  and the global cap bounds all providers together. Deadlines live
  on the tokio clock, so paused-time tests pin the timing
  deterministically. The `[ratelimit]` table gains load-time
  validation: zero lanes, a zero backoff base (the storm the jitter
  exists to prevent), and a negative or non-finite jitter (`inf`,
  `NaN`) reject (pinned by the bounded-jitter, host-isolation,
  retry-after-scaling, header-less-trip, clamped-cooldown, per-host
  lane, global-cap, and degenerate-knob tests). The judge pass is
  the consumer; nothing wires the gate into a call site yet.
- The flow-map builder (`src/judges/flow.rs`, type in
  `src/contracts.rs`): `flow::build(region, region_text, gateway,
  max_callers)` derives a region's behavioral surface from the
  pinned head — defines (the `fn` token in any modifier shape,
  including impl/trait methods), callers (`search_at_head` hits
  minus the region's own definition lines), callees (call tokens
  minus keywords and own defines), and trigger sites (caller hits
  carrying `.spawn(`/`::spawn(`/`.await`/register-style markers,
  plus `fn <name>(` sites outside the region — trait impls). A
  same-name re-definition lists one define while every definition
  line is excluded from its callers; line comments, nesting-aware
  block comments, and string, char, and `#`-counted raw-string
  literals are stripped before any tokenizing (a small state
  machine), so commented-out or quoted code never defines or
  calls — an unterminated string literal carries across the line
  break with the escape cleared, since Rust strings span lines —
  and `in` sits in the keyword denylist. Every arm sorts
  by (file, line),
  dedups, and caps at `max_callers`; a region with no defines
  builds an empty map without touching the gateway; a failed
  search fails the run (`FlowMap` carries serde derives for the
  judge-pass trajectory). Pinned by the determinism, cap/sort/exclusion,
  trigger-trio, scan, fast-path, serde round-trip,
  re-definition, comment/string-stripping, comment-nesting,
  char-literal/lifetime, raw-string, and string-carry tests.
- The coverage planner (`src/judges.rs`, wiring in `src/cli.rs`):
  `plan(range, regions, aspects, focus, severity)` builds the work
  order — cells = each region's `aspects_for` set (per-path
  overrides respected), sorted by aspect id within region-index
  order, deduplicated, deterministic for identical inputs. A
  focus list cuts cells outside it and a severity floor cuts
  cells whose aspect ceiling cannot clear it; either cut marks
  the plan `narrowed` (the new `CoveragePlan.narrowed` flag — an
  empty plan from no regions is not narrowed, nothing was cut),
  and the planner is pure: the flag is the only channel to the
  cross-round state. An unknown focus id errors naming it rather
  than silently emptying the plan. `summarize` estimates the
  round — judge calls = regions × judges, tokens ≈ region spans
  capped at 2000 lines × 12 tokens × judges (repo-spanning
  regions hit the cap, never an absurd estimate) — and
  `render_dry_run` prints the plan, estimate, pinned range, and
  per-region aspect outline. A local `difftrace review --dry-run` is
  now live end to end: it resolves config, opens the local gateway
  at the base (hosted `--repo/--pr` arguments stay not-yet — a
  hosted dry run errors instead of silently planning the local
  checkout), builds regions per scope (Full from the tracked file
  list), plans, and writes the report through a writer seam on a
  locked stdout — a closed pipe (`| head`) exits 0 silently, any
  other write failure fails as infrastructure (exit 2) via the new
  `DtError::Output`, never std's `print!` panic (exit 101); a real
  review still reports not-yet (pinned by the partition, narrowed,
  focus, severity, estimate, dry-run, dispatch-exits-clean,
  hosted-dry-run, full-scope, and write-seam tests).
- The repository gateway seam (`src/gateway.rs` +
  `src/gateway/local.rs`): the `PrGateway` trait — `pinned_range`,
  `diff`, `read_file_at_head`, `list_files_at_head`,
  `search_at_head` — and the local hermetic-git implementation:
  every invocation carries `-c color.ui=never
  core.quotePath=false diff.noprefix=false`, the repo root comes
  from `rev-parse --show-toplevel` (reads from any subdirectory
  stay repo-relative), and the pin is made once at open — base =
  `merge-base -- <base> HEAD` (the `--` keeps a flag-shaped `--base`
  value a revision, so it fails naming itself instead of silently
  selecting a git mode), head = `rev-parse HEAD`, diff =
  `git diff --no-renames base head` — so a commit after open
  cannot change the reviewed diff. File reads run `git cat-file blob
  head:path` (a missing, empty, or directory path errors naming the
  path — a tree listing never reads as file content), listing is
  `ls-tree -r --name-only` at head (sorted, tracked only; a failed
  listing errors instead of reading as an empty repository), and
  search is `git grep -n -I -e` POSIX BRE at the pinned sha —
  `grep.patternType`, `ignoreCase`, `column`, and `fullName` are
  pinned in the hermetic prefix so no repository or global config
  can flip the dialect or the hit format, and every git child
  strips inherited `GIT_*` environment variables so a stray
  `GIT_DIR` cannot redirect a read (the `-e` keeps a dash-leading
  pattern a pattern) — hits
  sorted by file then line and capped; colons in file names survive
  parsing, anchored on the first digit-only segment whose file part
  names a tracked file from the listing cached at open (leftmost as
  the fallback), so a digit-only name chunk never reads as the line
  number and digit-colon match text (`10:30`) never steals the
  anchor; no matches read as empty while a broken pattern
  errors. New contracts: `DiffSource
  { Unified }`, `FileSnapshot`, `SearchHit` (pinned by the
  pin-freeze, merge-base, subdir, sort-and-cap, colon-name,
  digit-name, digit-text, digit-colon-text, flag-shaped-base,
  dash-pattern, listing,
  failed-listing, tree-path, missing-file, no-repo, empty-read,
  hostile-git-config, and hostile-git-env tests).
- The Rust built-in adapters (`src/tools/rust.rs` +
  `src/tools/crates.rs`): fmt / clippy / check / audit as
  first-class `ToolAdapter`s over the workspace map — member
  directories (trailing-`*` globs expand by listing manifest-bearing
  subdirectories; other glob shapes reject) map changed paths to
  sorted, deduped crate names with unknown paths falling back to the
  root package; clippy/check build `cargo … -p <crates>
  --message-format=json` argv (`-D warnings` after `--` for clippy),
  fmt runs `cargo fmt --check` and reads its
  `Diff in <path>:<line>:` headers — one suggestion-grade finding
  per unformatted file at its first reported line, the path verbatim
  as the tool prints it — and audit reads `cargo audit --json`
  advisories as suggestion-grade findings at the `(workspace)`
  pseudo-target with the affected package and advisory id leading
  the message (state
  dedups by id later). Repeated enabled names reject (one tool
  label means one tool), and a zero `[tools] budget_secs` rejects
  at load. Built-in output passes through the same sort-and-cap
  normalization as external tools, so an interleaved clippy/check
  flood or a many-file fmt run still yields the same sorted, capped
  findings. `should_run` gates fmt/clippy/check on Rust
  paths and affected crates; the advisory scan always runs — the one
  non-diff-scoped exception. The run seam: an injectable `Executor`
  returning the drained stdout together with the exit status
  (`ProcessExecutor` reads stdout on a reader thread — output
  beyond pipe capacity cannot deadlock, the child is killed at the
  budget, and the post-exit drain is bounded by the same deadline
  through a channel, so a grandchild holding the pipe cannot hang
  the run past its budget), and `run_cached` keyed by
  `(tool, sha, Cargo.lock hash)` — a cache hit returns without
  invoking the tool, a corrupt cache re-runs, and a timeout, launch
  failure, unreadable output, or a failing exit that produced no
  findings marks the tool unavailable without failing the round and
  caches nothing, the reason naming the exit: a crashing run is
  never a clean run, while a failing exit with findings on stdout
  still runs — the normal diagnostic-tool shape (a failed read
  surfaces its error instead of returning partial text, so
  truncated findings are never cached for the sha), and cache
  writes are best-effort (pinned by the mapping, glob-rejection,
  virtual-workspace, argv, gating, enabled-list, fmt, audit,
  sort-and-cap, timeout, launch, read-failure, cache, echo,
  crash-exit, exit-with-findings, and bounded-drain tests).
- The tool adapter layer (`src/tools.rs` + `src/tools/diag.rs` +
  `src/tools/cargo_json.rs`): the tool-layer extension contract —
  `ToolAdapter` (name, `should_run` over changed regions, a data-only
  `CommandSpec` with argv/cwd/timeout, and `parse` into
  `ToolFinding`s) — implemented by external tools configured
  from `[[tools.external]]`: the command string splits quote-aware and
  expands `{files}` (a whole token becomes one argv element per
  target, an embedded occurrence joins them in place, and a target
  named like a flag is dropped rather than injected into argv),
  `run_on` matches changed files through the path glob with flat
  brace expansion (`**/*.{js,ts,tsx}`; patterns with nested,
  repeated, or unmatched braces reject at construction), and the
  entry's budget is the timeout. The `diag` parser reads the
  `file:line[:col]: level: message` convention shared by eslint's
  unix format, golangci-lint, clang-tidy, shellcheck, hadolint, and
  ruff — the level is optional in practice (golangci emits none,
  ruff emits a code); recognized levels map error/err/fatal →
  critical, warning/warn → warning, and info/note/style/performance/
  hint → suggestion, anything unrecognized keeps the whole message
  at warning; line zero rejects (timestamps and clock-stamped
  progress are not findings) and lines that do not match the shape
  skip as tool noise. The `cargo-json` parser maps cargo's
  `--message-format=json` compiler messages — errors and warnings
  only, anchored at the first primary span, with a spanless summary
  error surfacing at the `(workspace)` pseudo-target instead of
  vanishing — skipping notes, non-messages, and non-JSON lines. The
  registry sorts adapters by name, rejects unknown formats and
  duplicate names, and every adapter's output is sorted by file,
  line, message and capped at two hundred findings — deterministic
  run-to-run (pinned by the dialect, cargo-json, glob, argv,
  fail-closed, brace-rejection, flag-shaped-target, duplicate,
  sort, and cap tests).
- The round state core (`src/state.rs` + `src/state/file.rs` +
  `src/state/marker.rs`): the cross-round issue lifecycle — a re-find
  (anchored on the finding's first evidence cite and matched through
  the proven `same_spot` identity) resets misses and reopens retired
  or owner-rejected issues, a full covered round without a re-find
  grows misses and retires at two consecutive, partial or narrowed
  rounds freeze misses (absence is not evidence), and per-round vote
  history is kept capped at the recent twenty; two same-round findings
  at one spot record into one issue under the stronger vote count, and
  a same-round confirmed twin cannot undo a suppression's rejection —
  the owner's word holds for the round it fired in. Suppressions match on
  ≥50% title-token overlap plus the path glob over any evidence file,
  count a hit on every suppressed re-raise, and pin their issue as
  rejected; oracle-refuted claims and deterministic tool findings
  never enter the lifecycle. One struct, two serializers: the file
  backend (a sanitized `<checkout>.state.json` name so a hostile
  checkout label cannot escape the state directory, an atomic
  temp-then-rename write, a missing file loading a default, a corrupt
  one erroring, and a file saved for another checkout rejecting
  instead of loading) and the marker backend (a
  `<!-- difftrace:state … -->` block embedded in a verdict body with
  every `>` escaped as `\u003e`, so state can never close the host
  comment early; the block is the body's final line — extraction
  anchors to the tail, so a verdict body that quotes the marker
  prefix survives embedding and round-trips byte-for-byte, and a
  tail-shaped block that does not parse errors as corrupt rather
  than reading as stateless; re-embedding replaces the block).
  `Issue.misses` and
  `votes_seen` now default when absent, so earlier files keep parsing
  (pinned by the lifecycle, suppression, sanitization, escape,
  round-trip, body-quote, corrupt-tail, and legacy-parse tests).
- The aspect registry (`src/aspects.rs`): the design's check
  vocabulary as code — eight aspects with judge-facing definitions
  (verbatim from the design table) and mechanical severity ceilings
  (docs caps at suggestion and physically cannot emit a warning;
  tests/fixtures cap at warning; the rest allow critical), a
  config-editable run set (`[aspects] default`, with per-path
  `extra`/`replace` overrides resolved by the longest matching glob —
  `**` spans path segments, `*` stays within one, and a hand-rolled
  matcher keeps the dependency count unchanged), fail-closed
  construction that rejects unknown aspect ids wherever they appear,
  and `definitions_block` rendering the per-judge aspect list. The
  minimal registry that mechanical validation shipped as a
  placeholder is replaced wholesale: `validate_finding` now checks
  severity against this registry, unknown aspects keep their nitpick
  fail-closed ceiling, and both drop pins stay green (pinned by the
  ceiling, set-resolution, override, longest-glob, unknown-id,
  definition-block, and glob-semantics tests).
- The full config and CLI surface (`src/cli.rs`, `src/config.rs`): the
  complete command grammar — `review` with every design flag
  (`--base`/`--repo`+`--pr` mutual exclusions, `--scope`, `--focus`,
  `--severity`, `--judges`/repeatable `--judge`, `--passes`,
  `--oracle`, `--budget-secs`, `--config`, `--dry-run`, `--json`,
  `--fix-prompts`), plus `reject`, `suppress list|clear`, `state
  show|reset`, `reply --mode answer|plan`, and `eval run` — parsed by
  clap with typed vocabulary errors (unknown scope, severity, or
  oracle values reject with the valid set; non-UTF-8 argv still errors
  cleanly; an empty `--judge` or `--focus` value rejects at the
  boundary — an all-empty list must not silently widen to a full
  review — and an empty name among real names drops, with the env
  split mirroring both). Configuration loads layered — explicit `--config` path
  (required to exist), else the first `.difftrace/config.toml` found
  walking up from the working directory (whole-file precedence: a repo
  file replaces the global file; pinned so global tables cannot leak
  through), else `~/.difftrace/config.toml`, else defaults — and env
  overrides apply over the flag-mirrored knobs
  (`DIFFTRACE_SCOPE`/`_SEVERITY`/`_JUDGES`/`_JUDGE`/`_FOCUS`/`_PASSES`
  /`_ORACLE`/`_BUDGET_SECS`), resolved flag > env > file > default and
  pinned end to end. The v1 `~/.difftrace/env` file returns as the
  lowest env layer (comments, blanks, and malformed lines skipped;
  empty values count as unset on both layers), carried by an
  `EnvSource` seam so tests never mutate process environment; the
  live seam reads `vars_os` and skips non-UTF-8 variables rather
  than panicking, so a hostile environment cannot exit 101 before
  dispatch.
  `[[tools.external]]` entries parse per the design (name, command,
  `diag`|`cargo-json` format, `run_on` glob, per-entry `budget_secs`
  defaulting 60; unknown formats and zero budgets reject). Every
  subcommand except a local `review` still dispatches to a
  not-yet error at exit 2 — a hosted `--repo/--pr` review, `--dry-run`
  included, stays not-yet with the hosted gateway — and bodies arrive
  with their owning tasks (pinned by the flag-surface,
  mutual-exclusion, subcommand, dispatch, hosted-dry-run, layering,
  repo-config, env-override, env-file, non-UTF-8-env, focus-empty,
  external-tool, and knob-rejection tests). The
  `[review]` lifecycle knobs (`retire_after_misses`,
  `line_drift_tolerance`, `token_overlap_pct`) and the `[hosted]`
  table are absent until their owning stages wire them — setting one
  rejects as an unknown field.
- Mechanical finding validation (`src/contracts.rs`): the gate every
  finding passes before it may vote — cites must resolve in the pinned
  index, the mechanism must cite at least one line of its own region,
  external assumptions above suggestion drop, severity must sit under
  the aspect ceiling (a minimal registry over the default aspects:
  docs cap suggestion, tests/fixtures cap warning, the rest critical;
  unknown aspects fail closed), and titles must be non-empty within 80
  characters; each rule drops with its own reason (pinned per rule
  plus the all-five pass and the fail-closed unknown aspect).
- The diff layer (`src/diff.rs` + `src/diff/regions.rs`): a pure
  unified-diff parser (new-side line maps per file, `+++ b/` path
  handling, `/dev/null` deletions anchor nothing, hunk-body shape
  enforced with line-numbered errors), per-scope region building —
  added-line clusters with gap-merge three for the diff scope,
  hunk-spanning file regions for deep, repo regions from an injected
  file list for full — and the cite-resolution authority: a citation
  resolves only against lines the pinned diff actually shows, and
  clamping admits only lines inside the cited region. Parse shape is
  enforced both-sided: hunk headers only count outside bodies (an
  added line whose content starts with pluses stays body — no phantom
  files), old/new counts must both close on their own sides —
  added lines count against the new count, removed against the
  old, context against both, a body line past either declared
  count errors mid-hunk naming its line, and a zero old count
  parses new-file hunks — (under- and over-declared
  hunks and orphan hunks are line-numbered errors), `/dev/null`
  deletion bodies consume without anchoring, empty body lines read as
  context, and nested `b/` paths strip exactly one prefix. `\ No
  newline at end of file` markers are metadata in both scopes and
  anchor nothing, a malformed `@@` header errors naming its own line,
  an over-declared hunk names the final line, input that is not a diff
  at all rejects instead of parsing as an empty index, and deletion-,
  rename-, mode-, and binary-only commits parse as an empty index with
  nothing to review (pinned by the boundary, count, under-count,
  zero-old-count, orphan, tolerance,
  path, no-newline, malformed-header, non-diff, and empty-index
  tests).
- The data spine (`src/contracts.rs`): every pipeline stage
  communicates only through these types — pinned ranges, regions and
  coverage cells, judge reports with per-cell verdicts, findings with
  stable composite ids (`region/aspect/normalized-title`, built by
  the constructor so no caller forgets; case, spacing, and punctuation
  fold away), claim types, the reconciled verdict with coverage
  summary and cross-round state, and the structural-equivalence
  helper behind the idempotency metric (judge order normalizes away;
  everything else must match). `serde_json` returns to the main crate
  here — the `--json` verdict output is this module's round-trip,
  pinned byte-stably along with the id-stability, normalization,
  wire-tag, and equivalence tests; a title with no alphanumeric
  content keeps a distinct id through a stable digest of the raw
  text.
- The crate skeleton: the total rewrite begins. The v1 modules are
  gone from this branch (v1 stays runnable from master and its
  tags — the eval tooling targets the installed v1 binary); in their
  place stand the new module tree (identity carried over and wired in
  with ported pins), the design's error model
  (`DtError` with judge-absence and partial-round variants), the full
  configuration tables with the owner-answered judge panel
  (`zai:glm-5.3` + `zai:glm-5.3-flash` + `local`, quorum of two;
  oracle off by default) parsed strictly — unknown keys are rejected
  where v1 ignored them — and a subcommand surface
  (`review`/`reject`/`suppress`/`state`/`reply`/`eval`) that names
  what is not yet implemented and maps run outcomes to the exit-code
  contract 0–3 (pinned by the config-defaults, strict-parse,
  subcommand, and exit-code tests). The judge quorum lands as a named
  constant, argv tolerates non-UTF-8 bytes, `error_chain` walks real
  sources, and the crate metadata drops the loopctl claim and the
  unused `serde_json` dependency until they return.

### Changed

- The eval runner's `--binary` default now resolves `difftrace`
  from `PATH` instead of preferring `target/release/difftrace` in
  the working tree, so a stale release build no longer shadows the
  installed v1 binary the metrics target (pinned by the
  `v1_binary` default test in `eval/src/bin/eval-run.rs`).
- `tokio` gains the `rt` and `rt-multi-thread` features in the main
  dependency set: the binary builds a runtime to drive the async
  review path.

- Hostile-review round 2 (all twelve findings of the 2026-09-28
  review): the round state is returned from the review as pending and
  persists only after the report sink succeeds, so exit 2 is honestly
  state-untouched (pinned at the orchestrator seam and end-to-end over
  a failing sink); the differential tool cache key folds an argv
  digest, so a widened re-run at the same sha re-inspects the wider
  targets instead of serving the narrow cache (pinned); the pinned
  diff runs `--no-ext-diff --no-textconv`, so a repo's diff drivers
  cannot reshape the review authority (pinned with a textconv
  fixture); `example.config.toml` ships loadable as-is and is pinned
  by a load test; `orchestrator.rs` carries a `//!` header and a repo
  self-check walks `src/` asserting headers on every source file
  (pinned); the one stray per-item doc comment is gone; whitespace-
  only provider secrets reject loudly naming the variable instead of
  building a client that fails remotely (pinned); a failed untracked
  listing warns and doc-refs stays silent rather than mislabeling a
  dangling reference (pinned); `incomplete` collects through a set so
  repeated judge ids list once (pinned); `equivalent()` compares the
  incomplete set and lead verdicts (pinned); the `--base` mutual-
  exclusion error states merge-base semantics and promises no
  `origin/` fallback (pinned); planning scratch left the tree.

- Reviewer-quality round (the 14-fix ladder from the hostile-review
  gap analysis): a region that fails by exceeding its stream budget
  is never retried — the same wall cannot fit it — and logs as `region
  too large for its stream budget` (pinned); the per-region stream
  timeout is now per-call (`judges.stream_timeout_secs`), with the
  attempt budget scaling by the turns allowed and `RunConfig.max_turns`
  pinned to the tool budget, so tool-using regions are not doomed by a
  single-call ceiling (pinned); the `[ratelimit]` table is wired —
  per-provider lanes bound concurrent calls and a rate-limited
  response trips a per-provider cooldown all waiters respect (pinned);
  regions longer than `judges.region_max_lines` (default 400, the
  snapshot window) split into fitting sub-regions with ids and
  coverage preserved (pinned); gate-dropped claims render as
  `## Unverified leads (not findings)` with drop reasons, and a
  needs-execution lead is tried once by the oracle with its verdict
  riding the report (pinned); `judges.escalate_profile` re-judges every
  sub-quorum defect once on a second-opinion profile whose vote can
  confirm a solo finding (pinned); judges receive the working tree's
  untracked-file list in their materials, so a tracked doc referencing
  a file the branch does not carry is judgeable (pinned); the
  deterministic `doc-refs` stage-0 tool flags markdown references to
  files the branch does not track, naming the untracked-present merge
  fix (pinned); the aspect vocabulary gains `canon` (//! headers, no
  planning references, doc-claims-match-code) (pinned); an omitted
  `oracle.policy` resolves against the repo — `check` for cargo
  workspaces, `off` elsewhere — with explicit values always winning
  (pinned).

### Fixed

- The branch-wide hostile review pass (eight findings, all covered):
  - Hermetic git pins `diff.srcPrefix=a/` and `diff.dstPrefix=b/`: a
    user's `[diff] srcPrefix/dstPrefix` config can no longer silently
    re-key every file in the review — the pinned diff, the tracked
    list, and the head reads stay keyed by real paths (pinned against
    repo-local hostile prefixes).
  - The oracle treats a signal-killed command as `Inconclusive`, not
    `Confirmed`: an OOM or admin `kill` during a verification run can
    no longer promote a solo finding to blocking (pinned with a
    `code: None` executor).
  - A 429 carrying the provider's directive cools the gate for the
    directed interval, not the default first-attempt backoff: the
    `retry after …` value is parsed out of the classified error
    (loopctl's `Some(12s)` and stream-layer renderings both) and
    trips the gate with it (parser pinned on both renderings).
  - An explicit `--budget-secs` bounds the whole run again: the wall
    clock starts before stage-0, and the tool layer's deadline is the
    tighter of its own budget and the run's (arithmetic pinned; a
    sleep-5 tool under a 2s wall dies on the wall, pinned end to
    end).
  - `region_diff` ends a file's window at the next file's
    `---`/`+++` header pair even in separator-less diffs, so another
    file's headers never masquerade as this file's body (pinned; the
    mid-hunk `+++ oops` body pin stands).
  - Coverage counts dropped findings by instance (judge + id), so one
    judge's invalid instance can no longer erase another judge's
    valid vote from the matrix — the confirmed list and the per-file
    table agree (pinned with a three-judge shared-id fixture;
    `Dropped` carries its judge on the wire, serde-default).
  - Style: every multi-line inline comment inside a body compressed
    to the canon's two-line budget, and the one verb-phrase test name
    renamed to a sentence.
- The second hostile-review pass of 2026-10-28 (ten findings, all
  covered):
  - A mechanism carrying a link or image no longer renders live in
    the confirmed-findings list: inside a list item the indented
    mechanism is a lazy continuation, so it escapes like every other
    model-controlled inline field (the fix-prompt block keeps its
    code-block mechanisms; pinned with link- and image-shaped
    mechanisms).
  - The reconcile fold is deterministic: judge reports enter in judge
    id order, so panel completion order cannot move a group boundary,
    a severity median, or the state's new-issue order — two runs on
    identical inputs stay equivalent at any panel size (pinned with a
    permutation pair).
  - The escalation sweep runs under the reserved id
    `<profile>#esc` (`#` stays banned in user profiles), so an
    escalation sharing a profile with an absent or incomplete panel
    judge can never answer for it in the presence and coverage
    accounting — a transient outage keeps exiting 3 (pinned).
  - The escalation sub-quorum test counts distinct profiles like the
    fold itself, so a replicated panel's genuinely-solo defects
    escalate for an independent profile to confirm (pinned).
  - The pinned diff parser rejects a header-shaped line that closes a
    hunk's counts exactly — an under-declared hunk can no longer eat
    the next file's `---`/`+++` pair — and a stray `+++` outside a
    hunk without its `---` pair errors instead of minting a phantom
    file (both line-numbered, pinned). Genuine content that starts
    `-- `/`++ ` and happens to close a hunk exactly is indistinguishable
    from the swallow and fails closed the same way — the diff format
    itself cannot tell the two apart (pinned).
  - `example.config.toml` is the pure compiled defaults again
    (`tools.budget_secs` 120, the aspect-override demo moved to
    comments), pinned by field-for-field equality against
    `DifftraceConfig::default()`.
  - A failed base export (no `tar`, unwritable temp, unresolvable
    base sha) degrades every adapter to `unavailable` naming the
    export cause instead of failing the run — tool unavailability is
    logged and never fatal, as the CHANGELOG always claimed (pinned).
  - `tools.budget_secs` is now the shared wall it always said it was:
    one layer deadline bounds head and base sides of every adapter
    together, each side clamped to the remainder, a spent budget
    skipping the side with a reason naming the share (pinned at the
    arithmetic and through a real timed run).
  - The tree ships no planning references: the eighteen `(#N)` review
    markers are gone from source and tests, and the repo self-check now
    rejects per-item `///` doc comments wherever it checks `//!`
    headers, making the no-item-docs rule enforced rather than asserted
    (pinned).
  - The branch's planning artifacts cannot ride the merge:
    `DESIGN-*.md` and the `hostile-review-*.md` notes are gitignored,
    so a blanket `git add` cannot commit files the canon keeps out of
    the repository.
- The hostile-review round of 2026-10-28 (twelve findings, all
  covered):
  - **Cite authority follows the materials** (HIGH): a `File` or
    `Repo` region's judge materials carry the head snapshot truncated
    to a 400-line window (200 head + 200 tail; whole file when it
    fits), and cite validation admits exactly the lines that window
    can contain for those region kinds (`CiteAuthority { index,
    head_lines }` threaded from the orchestrator through `reconcile`,
    window-aware), while `Added` and `Hunk` regions keep the
    diff-only authority byte-for-byte; an unreadable head snapshot
    fails closed to diff-only (pinned).
  - An empty line inside a hunk is a context line: `region_diff` no
    longer lets it end the file's region, so later hunks stay
    reachable in the judge's materials (pinned).
  - Panel identity is unique: a profile filling a second panel slot
    or running a second pass gets the id `profile#2`, `#` is banned
    in profile names, and the quorum and severity median count one
    vote per distinct profile — one model duplicated across slots
    can no longer fake a quorum (pinned; an under-quorum panel warns,
    helper pinned).
  - Judge evidence cites split at the **last** colon (Windows paths
    map), and cite candidates that will not parse are counted onto
    `Finding.discarded_cites` and drop under the new
    `unparseable-evidence` reason instead of vanishing (pinned).
  - Differential attribution tolerates line drift: a tool finding
    with a base twin within five lines is repo debt, and "new" is
    reserved for a message with no twin near it (pinned); untracked
    files no longer skip the tool layer (`git status -uno`), while a
    modified tracked file still fails the head side closed (pinned);
    tool-unavailability reasons now ride the verdict as
    `tools_unavailable` (serde-default; legacy reports parse) and
    render as a disclosure line (pinned).
  - File paths are `escape_inline`-d in every render position —
    cite anchors, tool findings, per-file coverage — closing the
    crafted-file-name link injection (pinned in the inert-render
    suite).
  - A cooling-down provider delays every region's next model call —
    the first attempt included — through the gate's `wait`, bounded
    by the wall budget, and the accumulated wait is logged per sweep
    (pinned); `DtError::OracleBudget` and the empty `reply` module
    are deleted as unreachable surface.
  - The gateway header no longer claims hosted GitHub ships on this
    branch; the defaults test loads its config with `?` so it can
    actually fail; the binary e2e spawns through an `env_clear`
    scrub (PATH and HOME only) so ambient `DIFFTRACE_*`/`RUST_LOG`
    cannot flip its premise (pinned); state saves stage through a
    pid-and-nanos-unique temp name and never touch a foreign file at
    the old shared name (pinned).
- The third hostile review of the branch (2026-10-29, ten findings,
  all covered):
  - Verdict equivalence compares the weak-lead tier: `equivalent()`
    sorts both sides on (judge, finding id, reason) — `DropReason`
    derives `Ord` for the key — so a run that lost or re-reasoned a
    weak lead is drift the idempotency metric can see, while a pure
    reordering stays equivalent (pinned by
    `equivalent_respects_weak_leads_and_ignores_their_order`).
  - Judge tool repeat notices name their target (`unchanged (ask #2
    of src/a.rs)`), so second asks of different files can never share
    text and re-form the loop detector's identical-answer triple —
    exactly the stubborn-judge shape the notice absorbs (pinned by
    `repeat_notices_for_different_targets_never_share_text`).
  - `region_diff` is count-aware: a genuine `--- `/`+++ ` body pair
    inside the declared hunk counts stays in the judge's materials
    (previously every line after it was silently dropped while the
    index still anchored them), and only a pair at counts-exhaustion
    ends the file — the separator-less next-file pin now rides a
    well-formed diff (pinned by
    `a_genuine_header_shaped_body_pair_mid_hunk_keeps_the_window`).
  - A file emptied but not deleted contributes no deep-scope region:
    its `+0,0` hunk anchors nothing, so no judge sweep reviews a
    region where every cite must fail (pinned by
    `an_emptied_file_contributes_no_deep_scope_region`).
  - File/Repo cite authority is truncation-aware: beyond the diff's
    own lines it admits only the 400-line snapshot window the judge's
    materials actually carry (200 head + 200 tail; the whole file when
    it fits), so a cite into never-seen middle lines weakens instead
    of validating (pinned by `the_snapshot_window_admits_only_its_own_lines`
    and `a_file_region_admits_only_the_snapshot_window_for_long_files`;
    the earlier authority bullet above is amended to this shape).
  - Timestamped `+++` headers key the bare path in both the index and
    the materials — `new_path` splits the tab exactly as `region_diff`
    always did (pinned by `a_timestamped_header_keys_the_bare_path`).
  - A zero passed through `--passes` is reported against the flag,
    not `DIFFTRACE_PASSES` — the knob that supplied the value is the
    knob the error names (pinned by
    `a_zero_from_the_flag_blames_the_flag_not_the_env`); clap usage
    errors keep exit 2 but print labeled `usage error:` so a typo
    never reads as a broken environment, documented in the README
    (pinned by `a_parse_failure_renders_as_a_labeled_usage_error`).
  - The example config's `region_max_lines` comment states the real
    default (3000) and names the 400-line snapshot window separately;
    the dependency-floor integration test folds its per-item docs
    into the module header per the house canon.

## [0.11.4] - 2026-09-08

### Added

- A mutation-set generator beside it: `eval-mutate` validates a
  local corpus manifest (`eval/mutations/manifest.toml` — gitignored
  like the golden index, it pins the owner's private eval selection of
  base commits, target lines, and recipes) against the sibling
  checkouts read-only, and `--apply N` materializes one case in a
  scratch clone (`~/.difftrace/eval-work/`) as a single commit on top
  of its base sha — the review diff is exactly the seeded defect,
  with `--check 1` adding a `cargo check` gate (pinned by the recipe
  transform, anchor-uniqueness, manifest-rejection, and synthetic
  apply-range tests; `make eval-mutations` validates every case in
  the local manifest). The eval package's one new dependency is
  `toml`, intrinsic to the manifest format; malformed entries —
  including short shas — are typed errors, targets with CR bytes are
  refused rather than renormalized, and files without a trailing
  newline keep their termination so the review diff stays exactly the
  seeded defect.
- An eval package at `eval/` with the `eval-golden` binary: a
  golden-set extractor that reads the local review transcripts
  (`~/.difftrace/reviews`) and labels every posted finding against git
  history — `true-positive` when a descendant commit of the reviewed
  sha touches the finding's file, `false-positive` when an entry in
  `eval/seed-false-positives.md` matches (owner-verified refutations
  override fix correlation), `unknown` otherwise. The labeled index is
  written locally to `eval/golden/index.json` — gitignored, it embeds
  private review history — with a per-repo distribution printed;
  `make eval-golden` regenerates it, and `make ci` now formats, lints,
  and tests the package (pinned by the transcript parsing, era,
  prose-negative, seed-override, and synthetic git correlation tests). Grouped report rows rendering
  `Title — also at `other:line`` no longer double-count their
  finding: the title is cut at the `also-at` suffix before parsing,
  and the grouped fixture row deduplicates against its plain verdict
  twin (pinned by the finding-count assertion in the log-era fixture
  test).
- Three review scopes and a focus control on `difftrace review`:
  `--scope diff` (the new default) reviews only the changed lines of the
  changed files — evidence packs become hunk windows, and observations
  outside the diff are suppressed with a one-line count telling you how
  to see them; `--scope deep` reviews the whole content of every changed
  file and files everything the diff cannot anchor into a separate
  "Existing issues — not from this change" section with its own fix
  prompt, non-blocking and outside the registry; `--scope full` audits
  every tracked file in the repository (local `--base` runs only,
  batches of at least four files, no miss-hunt — a weekly hygiene pass,
  not a per-round tool). `--focus security,correctness,concurrency,
  performance,tests,docs` narrows what the reviewer hunts, and
  `--severity` drops milder findings unseen with a counted note.
  Both also work as `review.scope`, `review.focus`, and
  `review.severity` in the config file and `DIFFTRACE_SCOPE`,
  `DIFFTRACE_FOCUS`, and `DIFFTRACE_SEVERITY` overrides (pinned by
  the scope/focus/override tests in `config`, `cli`, `rubric`,
  `evidence`, and `submit`, the existing-issues and suppression tests in
  `prompts`, and `an_unmatched_out_of_hunk_finding_never_becomes_a_registry_issue`).
  A narrowed round (`--focus` or `--severity` set) also never retires
  registry issues it could not have re-raised — absence is not evidence
  of a fix when the reviewer was not looking for that kind of issue
  (pinned by `a_narrowed_round_never_retires_issues_it_cannot_re_raise`;
  manually resolved threads still record their resolution).
- Local rounds always run the miss-hunt: the second adversarial pass
  previously skipped itself whenever the first pass found any blocking
  finding, so rounds with warnings got one sample instead of two — and
  per-pass recall is roughly a third, which is why every round surfaced
  "new" warnings on unchanged code. Local `--base` rounds now always
  take the second pass (`review.hunt_always` forces it; hosted rounds
  keep the skip), and the hunt rubric no longer claims the first pass
  was clean when it was not — the hunt arm of the batch prompt makes
  the same honest claim ("whether or not it recorded findings") that
  the rubric does (pinned by
  `a_blocking_first_pass_still_hunts_when_hunt_always_is_set`, the
  hunt-rubric wording assertions, and
  `the_hunt_prompt_makes_no_cleanliness_claim_about_the_first_pass`).
- A local findings ledger makes repeated rounds converge (and suppressed
  re-reports count as presence): findings the model re-reported at lines
  the diff no longer marks as added — dropped by strict anchoring —
  reset the ledger's miss counter instead of accruing toward a false
  `fixed`, so scope suppression and line drift can never retire an
  issue that is still being reported (pinned by
  `a_suppressed_refind_on_an_unchanged_line_prevents_retirement`). every local
  review merges its findings into
  `~/.difftrace/reviews/<checkout>.ledger.json` with fuzzy identity
  (same file, line within ±5, majority token overlap), so a rephrased
  re-find at a shifted line is the same issue, not a new one. The round
  now reports `new / fixed this round / standing (fixed all-time)` —
  fixed means the issue stayed unfound across two consecutive covered
  rounds — one absence is not evidence at a third-per-pass recall;
  partial rounds never retire; a fixed issue that reappears reopens
  instead of duplicating (pinned by the ledger merge tests).
  Per-run recall is roughly a third, so without the ledger each round
  re-samples a different subset and the count fluctuates instead of
  shrinking; with it, standing decreases geometrically per fix wave.
- Findings now carry a structured `kind` (security, correctness,
  concurrency, performance, tests, docs) on the wire, and two contracts
  are enforced mechanically instead of by prompt: `kind: docs` findings
  are capped at suggestion severity no matter what the model stamped —
  documentation can never block a round — and `--focus` now drops
  out-of-focus findings in grounding with a counted note instead of
  only steering the rubric (pinned by the grounding tests).
- Diff scope anchors findings to added lines only: a finding citing an
  unchanged context line inside a hunk (or a removed line) is now
  dropped like an out-of-hunk one and counted in the suppression note,
  instead of riding through as a round finding — "review the diff"
  now means the lines the diff actually added, not the ±3 context
  around them. Deep scope keeps whole-hunk anchoring and full scope
  still anchors anywhere (pinned by
  `diff_scope_drops_findings_on_unchanged_context_lines` and
  `deep_scope_still_admits_hunk_context_lines`).
- The first self-review round (difftrace reviewing its own `--scope`
  work) caught three defects, all fixed: the local run log recorded the
  config-file scope instead of the `--scope` flag actually executed;
  whole-file evidence packs labeled every large file "the file has no
  parseable hunks" (a false coverage statement for the reviewer model —
  the header now names the real reason: "whole-file mode requested");
  and `SubmitReviewTool` hard-coded diff scope and no severity floor on
  the tool-based submission path, ignoring the configured knobs — both
  now thread through `ReviewScope::registry` (pinned by
  `the_configured_floor_reaches_the_submit_tool` and the whole-file
  header assertions).
- Documentation findings are suggestion-grade by default: an imprecise,
  incomplete, or stale comment is never a warning; warning is reserved
  for a doc claim that misstates a behavioral contract a caller would
  build against (safety, blocking, cancellation, lifetime, ordering) —
  trusting the doc would ship a defect. Doc-accuracy warnings were the
  main source of freshly-minted blockers on fix code (pinned by the
  rubric assertion in `the_rubric_carries_rules_and_frame_every_turn`).
- A `local` provider profile for any OpenAI-compatible server —
  llama.cpp, vLLM, LM Studio, Ollama behind a proxy: requires
  `provider.model` and `provider.base_url`, sends no key unless
  `LOCAL_API_KEY` is set, and fails with the exact endpoint to configure
  when either is missing (pinned by `a_local_profile_requires_an_explicit_endpoint`
  and `a_local_profile_builds_at_any_openai_compatible_endpoint`).
  The new `DIFFTRACE_BASE_URL` override points any profile at a
  different endpoint for one run without touching the config file
  (pinned by `the_base_url_env_override_replaces_the_configured_endpoint`).
- A verbosity knob: `log_level` in the config file and the
  `DIFFTRACE_LOG` override (`error`–`trace`, default `info`; works in
  `~/.difftrace/env` and in workflow env blocks; `RUST_LOG` still wins
  as the escape hatch, and a bare `debug`/`trace` there is clamped the
  same way — only directive forms like `hyper=trace` pass through raw,
  so no spelling of "debug" ever drowns the log in connection chatter
  — pinned by the `filter_directive` tests). At `debug` the logs carry
  timing spans —
  every round, fetch, batch, verification, and summary pass closes a
  span with its busy-time — plus per-turn timing (total, tools, model ≈
  turn minus tools) from the logging observer, the resolved config,
  prompt/evidence sizes, registry merge decisions per issue, and
  per-finding verification verdicts (pinned by
  `a_span_close_line_carries_the_busy_time`, the `DIFFTRACE_LOG`
  override tests, and binary-verified at `DIFFTRACE_LOG=debug`). The
  reqwest/hyper/rustls connection chatter is held at `info` so debug
  stays readable.
- Every local review run writes a full transcript to
  `~/.difftrace/reviews/<checkout>-<timestamp>-<short sha>.log`
  (the reviewed checkout's directory name and a local-time timestamp,
  so runs of different repositories never blur together): the run's
  diagnostics (progress, retries, warnings — the subscriber tees
  stderr to the file) followed by the round body and the
  standing-comment render, or the error chain when the run fails; a
  logging failure never fails the review. Every run now opens by
  naming its version — the first progress line on screen, the
  transcript header, and the first log line — so a saved transcript
  always names the difftrace that wrote it (pinned by
  `a_local_review_log_name_carries_checkout_time_and_head` and
  `the_tee_reaches_the_file`). The local progress lines moved from
  bare stderr prints to the tracing subscriber so they land in both. The branch's one new main-crate dependency is `chrono` (clock feature only): transcript and log file names carry local-time stamps, which std cannot format.
- `~/.difftrace/env`: an optional `KEY=VALUE` secret file beside the
  config — the lowest-priority source for provider API keys,
  `GITHUB_TOKEN`, and every `DIFFTRACE_*` override, so local runs need
  no shell exports. A variable already in the environment always wins
  (CI and per-run overrides untouched); empty values count as unset on
  both layers; the config file itself stays secret-free and
  committable. The missing-key error now names both sources ("set
  ZAI_API_KEY in the environment or ~/.difftrace/env") (pinned by
  `the_env_file_parses_keys_and_skips_noise`,
  `the_environment_wins_over_the_env_file`,
  `a_profile_override_can_come_from_the_env_file`, and the
  `build_client_with` file-key tests in the provider suite).
- `difftrace review --base <ref>`: a fully offline pre-push review of
  `merge-base(<base>, HEAD)..HEAD` from the current checkout — the
  diff and file contents come from local git, no GitHub token is
  demanded, nothing is posted, and the round renders to stdout. The
  hosted rounds on loopctl#112 and dch#25 ran 11–61 minutes each and
  collided on the shared provider key when both repositories pushed
  within a minute; the local loop runs the same review before the
  push, against a head that cannot move mid-round, so what it clears
  stays cleared and the pull-request round that follows confirms
  instead of re-raising (gateway pinned by `the_local_diff_is_the_branch_delta`
  and friends in `local::tests`; CLI pinned by
  `a_local_review_parses_with_the_base_flag_alone`).

### Fixed

- The local ledger honors the same narrowed-round and presence rules
  the hosted registry does: a round with `--focus` or a `--severity`
  floor cannot retire issues it was not looking for (absence is not
  evidence of a fix), and every dropped re-report — grounding-cap
  drops included, not just out-of-hunk and unchanged-line — resets the
  miss counter as presence evidence, so an issue the model keeps
  re-finding can no longer retire as "fixed" while ranked sixth for
  its file. Both state keepers now share one predicate,
  `ReviewSettings::is_narrowed`, and the local round's merge inputs
  (all-drops presence, retirement gate) live in
  `ledger::local_merge_inputs` instead of the binary (pinned by
  `a_round_with_a_focus_or_a_severity_floor_is_narrowed` and
  `the_local_merge_inputs_count_every_drop_as_presence_and_gate_retirement`).
- A partial round no longer marks standing issues on unreviewed files
  as `Fixed` in the registry: `RoundFindings` now carries the round's
  unreviewed files and `retire_unseen` leaves their issues open —
  "not re-raised" cannot read as "fixed" for a file the round never
  saw, so provider-failure rounds stop falsifying issue history and
  resetting finding identity on the next full round (pinned by
  `an_open_issue_on_an_unreviewed_file_survives_a_partial_merge`).
  Unreviewed batches now travel the pipeline as structured file lists
  instead of `", "`-joined display strings re-split at the thread
  slate, so a filename containing the separator cannot corrupt the
  unreviewed set.
- The partial-review banner's literal whitespace run before "(provider
  failures)" collapsed to a single space (pinned by the exact-spacing
  assertion in `a_partial_round_leads_with_a_loud_banner`).
- A summary that never matches the schema after every corrective
  retry no longer fails the run: the summary pass now falls back to
  the same plain machine-written summary as the provider-unreachable
  path, so no post-findings failure mode can kill the round — only
  posting to GitHub can (pinned by
  `a_summary_still_malformed_after_the_retries_falls_back_to_a_plain_summary`).
- A round where every batch goes unreviewed now fails instead of
  posting: the loopctl#112 rate-limit round posted a hollow clean
  review ("0 findings") and resolved 11 previous threads because
  "not re-raised" read as "fixed" — with zero files reviewed, the
  round now errors out before posting anything or resolving any
  thread (pinned by
  `a_round_where_every_batch_fails_posts_nothing_and_resolves_no_threads`).
- Threads on unreviewed files are never resolved in partial rounds
  either: only a file the round actually reviewed can conclude its
  issues fixed (pinned by
  `a_thread_on_an_unreviewed_file_is_never_slated_for_resolution`).
- The miss-hunt no longer announces "hunting for misses" when every
  batch went unreviewed — there is nothing to hunt (pinned by the
  subscriber assertion in
  `the_hunt_skips_batches_the_first_pass_could_not_complete`).
- The unreviewed-files note no longer claims "the reviewer run failed
  twice": batch retries have been configurable
  (`review.batch_attempts`, default 10) since this release, so the
  posted note now states the configured count ("failed after all N
  attempt(s)") (the note assertions in
  `a_batch_whose_runs_fail_is_recorded_unreviewed_and_the_review_continues`
  pin the singular and plural forms).
- The rate-limited round's root cause was two reviews bursting
  simultaneously (loopctl and dch pushed a minute apart — 20 concurrent
  streams on one provider account); a single review at 10 lanes passes
  cleanly, so the lane count stays at 10 and the all-unreviewed guard
  above is what makes the rare storm fail safely instead of
  destructively.
- A hunt-pass failure on a batch the first pass reviewed can no longer
  trip the all-unreviewed failsafe: the hunt only ever re-runs
  reviewed batches, so its failures now warn and leave the first-pass
  results standing instead of merging into the unreviewed set — an
  outage that starts mid-round degrades to a partial round instead of
  discarding a successful batch's findings under a false "provider was
  unreachable for the whole run" error (pinned by
  `a_hunt_failure_on_a_reviewed_batch_stays_partial`).
- The partial-review banner counts files, not batches: with
  `review.batch_files` above 1, one failed batch of several files no
  longer undercounts the gap as "1 file(s) could not be reviewed"
  (pinned by `the_partial_banner_counts_files_not_batches`).
- The plain-summary fallback names its true cause per path: the
  provider-unreachable path keeps "(provider errors after every
  retry)" while the schema path — where the provider answered on
  schema three times — says the summaries never matched the schema
  after every corrective retry; the risk-notes and tests fields carry
  the same honest cause (pinned by the per-path assertions in
  `a_summary_still_malformed_after_the_retries_falls_back_to_a_plain_summary`
  and `a_summary_that_never_arrives_falls_back_to_a_plain_summary`).
- The local `--base` review pins its materials at open: the gateway
  resolves the head commit and merge-base once, and every read —
  overview, numstat, diff — runs against that pinned pair, so a commit
  landing after the open cannot slip into the reviewed diff or shift
  the stamped head (pinned by
  `a_commit_after_open_does_not_change_the_reviewed_diff`).
- The hosted registry now converges the way the local ledger does, instead
  of fragmenting known issues into new threads: issue identity across
  rounds is file + nearby line (±5) + shared wording — the same line with
  any shared word, a line within five with half the wording in common —
  so a re-finding after fixes shifted lines or rephrased the title
  refreshes the standing issue instead of minting a duplicate (pinned by
  `a_rephrased_refind_at_a_drifted_line_refreshes_the_standing_issue`,
  `two_phrasings_at_the_same_line_are_one_issue`, and the negative
  `a_far_drifted_refind_with_a_reworded_title_is_new`). A retired issue
  the model re-finds reopens rather than duplicating (pinned by
  `a_fixed_issue_the_model_refinds_reopens_rather_than_duplicates`). An
  issue retires only after two consecutive full rounds stop re-raising
  it — at measured per-pass recall, one absent round missed a real
  issue two times in three, marking it falsely fixed and resolving its
  thread; GitHub thread resolution follows the same rule, so a first
  miss holds the thread open (pinned by
  `retire_unseen_resolves_through_the_threads_resolution_state`,
  `merge_marks_unregraunded_issues_fixed_with_the_round_sha`, and
  `a_first_miss_keeps_the_thread_open_until_the_issue_retires`). A
  thread displaced by a different issue at its anchor still retires
  immediately — replacement is presence, not absence — and a suppressed
  re-report (outside the hunks, or on an unchanged line) resets the miss
  count as presence evidence (pinned by
  `a_suppressed_rephrased_refind_resets_the_miss_count`). Registry state
  written before the miss counter exists parses with misses at zero
  (pinned by `the_registry_parses_state_from_before_the_misses_field`).

### Changed

- The default per-response output budget for the Anthropic/Zai profiles
  rises from 8192 to 32768 tokens: a thinking model (the default zai
  family) spends its reasoning inside `max_tokens`, so the old ceiling
  truncated final turns before `record_findings` ever fired and every
  affected batch failed. The Anthropic protocol requires a finite
  `max_tokens` — there is no unlimited value — so raise
  `provider.max_tokens` (or `DIFFTRACE_MAX_TOKENS`) further, up to the
  endpoint's ceiling (the README example uses 131072), for models that
  think longer.
- A budget-truncated batch is no longer retried: the cap physically
  prevented the verdict, so the retry ladder only re-spent the budget
  up to nine more times with 8–90 s waits (observed live: a glm
  thinking round burning 8192 tokens × 10 attempts per batch). The
  batch now lands unreviewed on its first truncation with a warning
  that names the knob (pinned by `a_truncated_batch_is_not_retried`;
  `a_truncated_batch_is_recorded_unreviewed_instead_of_approved`
  adapted to the no-retry shape).
- The default provider profile is `zai`: a bare run with no config file
  and no `DIFFTRACE_PROFILE` now resolves zai and asks for
  `ZAI_API_KEY` (or its `ZHIPUAI_API_KEY` alias) instead of
  `ANTHROPIC_API_KEY` — every deployed workflow already pins zai
  explicitly, so this changes the fresh-machine story only (amends the
  frontier-first default settled before the zai profile existed;
  pinned by `a_partial_provider_section_keeps_other_defaults` and
  `an_unset_or_empty_env_override_keeps_the_config`).
- `difftrace review --base` now rejects `--pr` the way it already
  rejects `--sha` (the local review has no pull request), and the
  `--memory` help states that appending happens on posted rounds only.
- The standing comment's issue history renders collapsed: the fixed and
  manually-resolved list now sits inside a `<details>` container whose
  summary names the count ("Fixed and resolved across N issue(s)"), so
  a long-lived pull request's verdict stays readable instead of growing
  a page of struck-through titles (pinned by the collapse assertions in
  `two_rounds_leave_a_registry_with_open_and_fixed_issues`).

## [0.11.3] - 2026-09-08

### Fixed

- A transient provider error on the summary request no longer kills the
  whole review after every batch completed: the summary pass retries
  three times with backoff and, if the provider stays unreachable,
  falls back to a plain machine-written summary naming the intact
  finding count — the same never-veto contract the verification pass
  already had. The loopctl#112 round that died at minute 21 (all nine
  batches finished, one ZAI network error on the final call, exit 1,
  nothing posted) is the case in point (pinned by
  `a_summary_survives_transient_provider_failures` and
  `a_summary_that_never_arrives_falls_back_to_a_plain_summary`).
- A failed `read_file_at_head` on a `mod.rs` path now names the sibling
  module root ("no mod.rs here: this repo uses the `foo.rs` + `foo/`
  layout; the module root is src/memory.rs — read that instead of
  retrying") — the 0.11.2 rubric instruction alone did not stop the
  retry loop (22 loop detections on one invented `src/memory/mod.rs` in
  the same failed round), so the tool error carries the correction
  mechanically (pinned by
  `a_failed_mod_rs_read_names_the_sibling_module_root`).

## [0.11.2] - 2026-09-08

### Changed

- The review rules forbid guessing file paths from convention and
  repeating a failed tool call: a loopctl#112 lane burned six turns
  re-reading a `foo/mod.rs` that exists only in the model's prior — the
  crate uses `foo.rs` + `foo/`. Paths must come from the diff and tool
  results, and a tool error is final for that exact call (pinned by the
  rubric assertion in `the_rubric_carries_rules_and_frame_every_turn`).

- The caller template skips comments on plain issues at the job
  condition (`github.event_name != 'issue_comment' ||
  github.event.issue.pull_request != null`) — `issue_comment` fires for
  plain issues too, and only pull-request comments carry commands, so
  those runs are dead weight (adopted from loopctl#112's caller; dch's
  caller already excluded them in its richer condition).
- The caller template's `pr` forwarding ignores comments on plain
  issues — `github.event.issue.number` is an issue number there, not a
  PR number, so the forwarding now requires
  `github.event.issue.pull_request != null` before using it (found by
  the reviewer on loopctl#112). The header also quotes the
  `` `release.sh pins` `` command and adds the callee's concurrency
  lanes to the enforcement checklist.
- Comment commands queue instead of being dropped: the command lane now
  declares `queue: max` (up to 100 pending commands, FIFO — GitHub's
  default replaces a pending command when a third arrives), and a
  `review` command moved to its own job so each lane keeps static,
  valid concurrency settings (`queue: max` may not combine with
  `cancel-in-progress: true`). The re-review job joins the review lane
  and cancels the in-flight review exactly as before; only the
  drop-the-pending-command behavior is gone (flagged by the standing
  review summary on loopctl#112). Callers must not add their own
  single-group gate — a caller gate re-serializes everything and
  reintroduces the drop; the template delegates all concurrency to the
  callee.

## [0.11.1] - 2026-09-08

### Changed

- The reference workflow raises the per-turn output budget to 131072
  tokens, matching the 128k tool-output cap so a thinking model's
  longest turns are never the thing that cuts a batch short.
- The caller template forwards the pull request number explicitly
  (`github.event.pull_request.number || github.event.issue.number ||
  inputs.pr`) instead of relying on the callee to branch on the event
  context for every non-dispatch trigger (raised by the reviewer on
  loopctl#112).
- The reference workflow reviews one file per batch across ten parallel
  lanes (`DIFFTRACE_BATCH_FILES=1`,
  `DIFFTRACE_MAX_PARALLEL_BATCHES=10`): every changed file gets its own
  agent conversation with the full per-file evidence budget, and a round
  bounds at the slowest single file instead of the sum of all of them —
  the single-batch round on loopctl#112 needed 43 minutes and its hunt
  pass carried six of seven findings because the shared conversation
  starved the first pass.

### Added

- Review progress streams into the run's step summary: milestone lines
  (model, batch and turn events, hunt, verdict, posting) append to
  `$GITHUB_STEP_SUMMARY` line-buffered, so the Actions Summary page
  shows what the reviewer is doing while the job runs instead of a
  sealed log until completion.

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
