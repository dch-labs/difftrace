#!/usr/bin/env bash
# The v2 release train, local half. The CI half lives in
# .github/workflows/release.yml: every v0.12.* tag push builds the
# release, publishes the exact-tag release, and moves the moving v0.12
# tag that consumer callers pin.
#
#   release.sh "<subject for the source commit>"
#       Cut a release: run the gate, commit the source changes, commit
#       the version bump, tag, push. The version bump (Cargo.toml,
#       Cargo.lock) and the CHANGELOG section must already be in the
#       working tree.
#
#   release.sh --check
#       Validate the release plan without touching anything: version,
#       exact tag, moving tag, changelog section, tree state. No gate,
#       no commit, no tag, no push.
#
# Fails fast — a red gate cuts nothing, and no tag or push happens if
# any step fails.
set -euo pipefail

# A release must run to completion without stopping on a pager.
export GIT_PAGER=cat GH_PAGER=cat PAGER=cat

cd "$(dirname "$0")"

read_version() {
  VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)
  if [ -z "$VERSION" ]; then
    echo "ERROR: cannot read the version from Cargo.toml" >&2
    exit 1
  fi
}

moving_tag() {
  MAJOR=${VERSION%%.*}
  REST=${VERSION#*.}
  MINOR=${REST%%.*}
  echo "v$MAJOR.$MINOR"
}

check_plan() {
  read_version
  TAG="v$VERSION"
  MOVING=$(moving_tag)
  echo "release plan:"
  echo "  version:     $VERSION"
  echo "  exact tag:   $TAG"
  echo "  moving tag:  $MOVING (moved to $TAG by the Release workflow)"
  if git rev-parse "$TAG" >/dev/null 2>&1; then
    echo "ERROR: $TAG already exists — bump the version before releasing" >&2
    exit 1
  fi
  echo "  exact tag:   not taken"
  if grep -q "^## \[$VERSION\]" CHANGELOG.md; then
    echo "  changelog:   has a [$VERSION] section"
  else
    echo "ERROR: CHANGELOG.md has no [$VERSION] section — prepare the changelog before cutting" >&2
    exit 1
  fi
  if git diff --quiet && git diff --cached --quiet; then
    echo "  tree:        clean"
  else
    echo "  tree:        dirty (the cut commits these paths: src tests examples README.md .github/workflows Cargo.toml Cargo.lock CHANGELOG.md)"
  fi
  echo "nothing was committed, tagged, or pushed."
}

case "${1:-}" in
"")
  echo "usage: release.sh \"<subject for the source commit>\" | --check" >&2
  exit 1
  ;;

--check)
  check_plan
  ;;

*)
  SUBJECT=$1
  read_version
  TAG="v$VERSION"
  MOVING=$(moving_tag)
  if git rev-parse "$TAG" >/dev/null 2>&1; then
    echo "ERROR: $TAG already exists — bump the version before releasing" >&2
    exit 1
  fi
  PATHS="src tests examples README.md"
  WORKFLOW=.github/workflows
  if git diff --quiet && git diff --cached --quiet &&
    [ -z "$(git ls-files --others --exclude-standard -- $PATHS $WORKFLOW Cargo.toml Cargo.lock CHANGELOG.md)" ]; then
    echo "ERROR: nothing to release in src/, tests/, examples/, README.md, the workflows, Cargo.toml, Cargo.lock, CHANGELOG.md" >&2
    exit 1
  fi
  if ! grep -q "^## \[$VERSION\]" CHANGELOG.md; then
    echo "ERROR: CHANGELOG.md has no [$VERSION] section — prepare the changelog before cutting" >&2
    exit 1
  fi
  make ci
  git add $PATHS "$WORKFLOW"
  git commit -m "$SUBJECT" -- $PATHS "$WORKFLOW"
  git commit -m "chore: cut $VERSION" -- Cargo.toml Cargo.lock CHANGELOG.md
  git tag "$TAG"
  git push origin HEAD "$TAG"
  echo "$TAG pushed. The Release workflow builds it, publishes the release, and moves $MOVING to the same commit."
  ;;
esac
