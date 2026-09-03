# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial crate skeleton: library and binary targets, CI pipeline, style
  configuration (clippy thresholds, strict lint table), and Makefile gates.
- Configuration loading (`DifftraceConfig`) from `~/.difftrace/config.toml`
  with typed errors; provider profile selection, `GitHub` endpoint override,
  and review tuning. Secrets are environment-only, never in the file.
- Provider factory (`build_client`) mapping the configured profile onto
  loopctl's Anthropic and OpenAI clients via the statically dispatched
  `DifftraceClient` enum, with the `Ollama` profile riding the OpenAI
  protocol at a local endpoint. Keys resolve from the environment only.
