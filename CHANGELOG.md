# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased — 0.12.0-alpha.1, the v2 rewrite branch]

### Added

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
  per-region aspect outline. `difftrace review --dry-run` is now
  live end to end: it resolves config, opens the local gateway at
  the base, builds regions per scope (Full from the tracked file
  list), plans, and writes the report through a writer seam on a
  locked stdout — a closed pipe (`| head`) exits 0 silently, any
  other write failure fails as infrastructure (exit 2) via the new
  `DtError::Output`, never std's `print!` panic (exit 101); a real
  review still reports not-yet (pinned by the partition, narrowed,
  focus, severity, estimate, dry-run, dispatch-exits-clean,
  full-scope, and write-seam tests).
- The repository gateway seam (`src/gateway.rs` +
  `src/gateway/local.rs`): the `PrGateway` trait — `pinned_range`,
  `diff`, `read_file_at_head`, `list_files_at_head`,
  `search_at_head` — and the local hermetic-git implementation:
  every invocation carries `-c color.ui=never
  core.quotePath=false diff.noprefix=false`, the repo root comes
  from `rev-parse --show-toplevel` (reads from any subdirectory
  stay repo-relative), and the pin is made once at open — base =
  `merge-base <base> HEAD`, head = `rev-parse HEAD`, diff =
  `git diff --no-renames base head` — so a commit after open
  cannot change the reviewed diff. File reads run `git cat-file blob
  head:path` (a missing, empty, or directory path errors naming the
  path — a tree listing never reads as file content), listing is
  `ls-tree -r --name-only` at head (sorted, tracked only; a failed
  listing errors instead of reading as an empty repository), and
  search is `git grep -n -I` POSIX ERE at the pinned sha — hits
  sorted by file then line and capped, colons in file names survive
  parsing, no matches reading as empty while a broken pattern
  errors. New contracts: `DiffSource
  { Unified }`, `FileSnapshot`, `SearchHit` (pinned by the
  pin-freeze, merge-base, subdir, sort-and-cap, colon-name, listing,
  failed-listing, tree-path, missing-file, no-repo, and empty-read
  tests).
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
  (`ProcessExecutor` drains stdout on a reader thread — output
  beyond pipe capacity cannot deadlock — and kills the child at the
  budget), and `run_cached` keyed by `(tool, sha, Cargo.lock hash)`
  — a cache hit returns without invoking the tool, a corrupt cache
  re-runs, a timeout or launch failure marks the tool unavailable
  without failing the round and caches nothing, and cache writes are
  best-effort (pinned by the mapping, glob-rejection, virtual-
  workspace, argv, gating, enabled-list, fmt, audit, sort-and-cap,
  timeout, launch, cache, and echo tests).
- The tool adapter layer (`src/tools.rs` + `src/tools/diag.rs` +
  `src/tools/cargo_json.rs`): the S0 extension contract —
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
  cleanly; an empty `--judge` value rejects at the boundary and an
  empty name among real names drops, mirroring `--focus` and the env
  split). Configuration loads layered — explicit `--config` path
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
  `EnvSource` seam so tests never mutate process environment.
  `[[tools.external]]` entries parse per the design (name, command,
  `diag`|`cargo-json` format, `run_on` glob, per-entry `budget_secs`
  defaulting 60; unknown formats and zero budgets reject). Every
  subcommand still dispatches to a not-yet error at exit 2 — bodies
  arrive with their owning tasks (pinned by the flag-surface,
  mutual-exclusion, subcommand, dispatch, layering, repo-config,
  env-override, env-file, external-tool, and knob-rejection tests). The
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
  files), old/new counts must both close (under- and over-declared
  hunks and orphan hunks are line-numbered errors), `/dev/null`
  deletion bodies consume without anchoring, empty body lines read as
  context, and nested `b/` paths strip exactly one prefix. `\ No
  newline at end of file` markers are metadata in both scopes and
  anchor nothing, a malformed `@@` header errors naming its own line,
  an over-declared hunk names the final line, input that is not a diff
  at all rejects instead of parsing as an empty index, and deletion-,
  rename-, mode-, and binary-only commits parse as an empty index with
  nothing to review (pinned by the boundary, count, orphan, tolerance,
  path, no-newline, malformed-header, non-diff, and empty-index
  tests).
- The v2 data spine (`src/contracts.rs`): every pipeline stage
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
- The v2 crate skeleton: the total rewrite begins. The v1 modules are
  gone from this branch (v1 stays runnable from master and its
  tags — the eval tooling targets the installed v1 binary); in their
  place stand the v2 module tree (identity carried over and wired in
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
