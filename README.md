# difftrace v2 (rewrite in progress)

difftrace reviews a diff the way a careful senior engineer does: every
run produces a **coverage matrix** (region × aspect → clean / defect),
findings are exceptions confirmed by a quorum of independent judges or
by mechanical verification, and the verdict is assembled mechanically —
no model-written summary polishing results into facts.

**This branch is the v2 skeleton.** The product surfaces arrive task by
task; the subcommand table (`review`, `reject`, `suppress`, `state`,
`reply`, `eval`) names what is coming and reports what is not yet
implemented. The exit-code contract is `0` clean, `1` blocking
findings, `2` infrastructure failure, `3` partial round.

- **v1** — the full first implementation with its README, config
  reference, and usage — lives on `master` and the release tags; the
  installed v1 binary keeps working.
- **What changed here** — see `CHANGELOG.md` (`[Unreleased —
  0.12.0-alpha.1]`).
- **Configuration** — layered: an explicit `--config` path (which must
  exist), else the first `.difftrace/config.toml` found walking up
  from the working directory (whole-file precedence over the global
  file), else `~/.difftrace/config.toml`, else defaults. The v2 tables
  (`[judges]`, `[aspects]`, `[tools]` incl. `[[tools.external]]`,
  `[oracle]`, `[ratelimit]`, `[review]`, `[hosted]`); unknown keys are
  rejected. Env overrides cover the flag-mirrored knobs
  (`DIFFTRACE_SCOPE`, `DIFFTRACE_SEVERITY`, `DIFFTRACE_JUDGES`,
  `DIFFTRACE_JUDGE`, `DIFFTRACE_FOCUS`, `DIFFTRACE_PASSES`,
  `DIFFTRACE_ORACLE`, `DIFFTRACE_BUDGET_SECS`); secrets never live in
  config files — env or `~/.difftrace/env` only.
- **Eval** — the `eval/` package (golden set, mutation corpus, metrics
  runner) measures v1 now and v2 later; its runner drives whatever v1
  binary you point it at (`--binary`, defaulting to PATH).

## Build and test

```
make ci        # fmt, check, clippy -D warnings, tests, docs
```

The eval targets (`eval-golden`, `eval-mutations`, `eval-baseline`,
`eval-run-v1`) live in the Makefile.
