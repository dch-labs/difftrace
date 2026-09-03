# difftrace

An AI pull-request reviewer for GitHub, built on
[loopctl](https://github.com/dch-labs/loopctl).

difftrace parses a pull request's unified diff into an index that decides
which lines may anchor a review comment: a citation outside the changed
hunks is rejected, never guessed into place. The agent loop that turns that
authority into posted reviews is under construction.

**Status:** early development. Not yet usable.
