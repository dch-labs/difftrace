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
- **Configuration** — `~/.difftrace/config.toml` with the v2 tables
  (`[judges]`, `[aspects]`, `[tools]`, `[oracle]`, `[ratelimit]`,
  `[review]`, `[hosted]`); unknown keys are rejected. Secrets never
  live in the config file.
- **Eval** — the `eval/` package (golden set, mutation corpus, metrics
  runner) measures v1 now and v2 later; its runner drives whatever v1
  binary you point it at (`--binary`, defaulting to PATH).

## Build and test

```
make ci        # fmt, check, clippy -D warnings, tests, docs
```

The eval targets (`eval-golden`, `eval-mutations`, `eval-baseline`,
`eval-run-v1`) live in the Makefile.
