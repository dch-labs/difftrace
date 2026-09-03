# difftrace

An AI pull-request reviewer for GitHub, built on
[loopctl](https://github.com/dch-labs/loopctl).

difftrace fetches a pull request's diff and changed files, drives an LLM agent
loop over them, and posts a single review whose inline findings are validated
against the diff before submission — a finding that cites a line outside the
changed hunks is dropped, never posted.

**Status:** early development. Not yet usable.
