#!/usr/bin/env bash
#
# sync-cardsfolder.sh
#
# Sync the current state of forge-gui/res/cardsfolder/ from the upstream
# Card-Forge/forge repository into this mirror's ./cardsfolder/ directory.
#
# Approach: sparse partial-clone of upstream (only the cardsfolder tree),
# then rsync --delete into ./cardsfolder/. This makes the mirror an exact
# snapshot of upstream's folder at whatever ref we check out — files added,
# modified, AND deleted upstream are all reflected. History is intentionally
# NOT preserved (see README "Why folder-sync, not git subtree").
#
# Usage:
#   scripts/sync-cardsfolder.sh [UPSTREAM_REF]
#
# UPSTREAM_REF defaults to the upstream default branch (master). Pass an
# explicit commit SHA / branch / tag to sync to a specific upstream state
# (used by the catch-up test to simulate upstream advancing).
#
# Environment overrides:
#   UPSTREAM_URL   default https://github.com/Card-Forge/forge.git
#   UPSTREAM_PATH  default forge-gui/res/cardsfolder
#   MIRROR_DEST    default cardsfolder   (relative to the mirror repo root)
#   WORKDIR        default a fresh mktemp dir (the upstream sparse clone)
#
# The script must be run from the root of the mirror repository.

set -euo pipefail

UPSTREAM_URL="${UPSTREAM_URL:-https://github.com/Card-Forge/forge.git}"
UPSTREAM_PATH="${UPSTREAM_PATH:-forge-gui/res/cardsfolder}"
UPSTREAM_REF="${1:-master}"
MIRROR_DEST="${MIRROR_DEST:-cardsfolder}"

# Resolve the mirror repo root (where this script's parent dir lives).
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [ ! -d .git ]; then
  echo "ERROR: must be run from the root of the mirror git repo (no .git found in $REPO_ROOT)" >&2
  exit 1
fi

CLEANUP_WORKDIR=0
if [ -z "${WORKDIR:-}" ]; then
  WORKDIR="$(mktemp -d)"
  CLEANUP_WORKDIR=1
fi
cleanup() {
  if [ "$CLEANUP_WORKDIR" = "1" ] && [ -n "${WORKDIR:-}" ]; then
    rm -rf "$WORKDIR"
  fi
}
trap cleanup EXIT

echo "==> Sparse-cloning upstream $UPSTREAM_URL (ref: $UPSTREAM_REF) into $WORKDIR"
UP="$WORKDIR/forge"
if [ ! -d "$UP/.git" ]; then
  git clone --filter=blob:none --no-checkout "$UPSTREAM_URL" "$UP"
  git -C "$UP" sparse-checkout init --cone
  git -C "$UP" sparse-checkout set "$UPSTREAM_PATH"
fi

echo "==> Fetching and checking out upstream ref: $UPSTREAM_REF"
git -C "$UP" fetch --filter=blob:none origin "$UPSTREAM_REF" || git -C "$UP" fetch --filter=blob:none origin
git -C "$UP" checkout --quiet --detach FETCH_HEAD 2>/dev/null || git -C "$UP" checkout --quiet "$UPSTREAM_REF"

RESOLVED_SHA="$(git -C "$UP" rev-parse HEAD)"
echo "==> Upstream resolved to $RESOLVED_SHA"

SRC="$UP/$UPSTREAM_PATH"
if [ ! -d "$SRC" ]; then
  echo "ERROR: upstream path $UPSTREAM_PATH not found at ref $UPSTREAM_REF" >&2
  exit 1
fi

echo "==> Syncing $SRC/ -> $REPO_ROOT/$MIRROR_DEST/ (rsync --delete)"
mkdir -p "$MIRROR_DEST"
# --delete makes the destination an EXACT mirror (removes files dropped upstream).
# Trailing slash on SRC copies its *contents* into MIRROR_DEST.
rsync -a --delete "$SRC/" "$MIRROR_DEST/"

# Record the upstream SHA we synced to, for provenance/traceability.
echo "$RESOLVED_SHA" > "$MIRROR_DEST/.upstream-sha"

echo "==> Sync complete. Mirror now reflects upstream cardsfolder at $RESOLVED_SHA"
