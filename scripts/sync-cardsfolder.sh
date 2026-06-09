#!/usr/bin/env bash
# sync-cardsfolder.sh
#
# Mirrors forge-gui/res/cardsfolder from Card-Forge/forge into ./cardsfolder/.
#
# Behaviour
# ─────────
# • First run (no .forge-upstream-sha):
#   Performs a flat import of the current HEAD of forge master and stores the
#   upstream commit SHA in .forge-upstream-sha.
#
# • Subsequent runs:
#   Replays every upstream commit (since the stored SHA) that touched
#   forge-gui/res/cardsfolder, in order, as a mirrored commit in this repo.
#   Each mirrored commit preserves the original author name/e-mail and date.
#
# Environment variables (all optional)
# ─────────────────────────────────────
# FROM_SHA    – Override the stored SHA and start syncing from this point.
#               Useful for testing the catch-up logic or manual recovery.
# MAX_COMMITS – Maximum upstream commits to apply in one run (default: 200).
#               If more are pending, subsequent runs will continue.

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
UPSTREAM_REPO="https://github.com/Card-Forge/forge.git"
UPSTREAM_PATH="forge-gui/res/cardsfolder"
DEST_DIR="cardsfolder"
SHA_FILE=".forge-upstream-sha"
FORGE_DIR="/tmp/forge-upstream"
MAX_COMMITS="${MAX_COMMITS:-200}"

# Always work from the repository root (parent of this script's directory)
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"

# ── 1. Set up / refresh the sparse upstream clone ────────────────────────────
if [ -d "$FORGE_DIR/.git" ]; then
    echo "Refreshing upstream clone..."
    git -C "$FORGE_DIR" fetch origin master --quiet
    git -C "$FORGE_DIR" checkout master --quiet
    git -C "$FORGE_DIR" reset --hard origin/master --quiet
else
    echo "Cloning upstream (sparse checkout: $UPSTREAM_PATH)..."
    git clone \
        --filter=blob:none \
        --no-checkout \
        --quiet \
        "$UPSTREAM_REPO" \
        "$FORGE_DIR"
    git -C "$FORGE_DIR" sparse-checkout init --cone
    git -C "$FORGE_DIR" sparse-checkout set "$UPSTREAM_PATH"
    git -C "$FORGE_DIR" checkout master --quiet
fi

# ── 2. Determine the sync starting point ─────────────────────────────────────
if [ -n "${FROM_SHA:-}" ]; then
    echo "FROM_SHA override – starting from: $FROM_SHA"
    LAST_SHA="$FROM_SHA"
elif [ -f "$SHA_FILE" ]; then
    LAST_SHA="$(cat "$SHA_FILE")"
    echo "Continuing from last sync: ${LAST_SHA:0:8}"
else
    LAST_SHA=""
    echo "No .forge-upstream-sha found; will perform initial import."
fi

# ── Helper: rsync current forge working tree → cardsfolder/ ──────────────────
apply_snapshot() {
    mkdir -p "$DEST_DIR"
    rsync -a --delete "$FORGE_DIR/$UPSTREAM_PATH/" "$DEST_DIR/"
}

# ── 3a. Initial flat import ───────────────────────────────────────────────────
if [ -z "$LAST_SHA" ]; then
    HEAD_SHA=$(git -C "$FORGE_DIR" rev-parse HEAD)
    echo "Importing cardsfolder at forge@${HEAD_SHA:0:8}..."

    apply_snapshot

    echo "$HEAD_SHA" > "$SHA_FILE"
    git add "$DEST_DIR/" "$SHA_FILE"

    if git diff --staged --quiet; then
        echo "Nothing to import (working tree already matches HEAD)."
    else
        git commit -m "Initial import of cardsfolder from Card-Forge/forge@${HEAD_SHA:0:8}"
        echo "Initial import committed."
    fi
    exit 0
fi

# ── 3b. Incremental sync ──────────────────────────────────────────────────────
mapfile -t COMMITS < <(
    git -C "$FORGE_DIR" log --reverse --format="%H" \
        "${LAST_SHA}..HEAD" -- "$UPSTREAM_PATH"
)

TOTAL="${#COMMITS[@]}"

if [ "$TOTAL" -eq 0 ]; then
    echo "Already up to date (last synced: ${LAST_SHA:0:8}). Nothing to do."
    exit 0
fi

if [ "$TOTAL" -gt "$MAX_COMMITS" ]; then
    echo "WARNING: $TOTAL commits pending; capping at $MAX_COMMITS per run."
    COMMITS=("${COMMITS[@]:0:$MAX_COMMITS}")
    TOTAL="$MAX_COMMITS"
fi

echo "Syncing $TOTAL upstream commit(s)..."

N=0
for sha in "${COMMITS[@]}"; do
    N=$((N + 1))

    msg=$(git -C "$FORGE_DIR" log -1 --format="%s" "$sha")
    author_name=$(git -C "$FORGE_DIR" log -1 --format="%an" "$sha")
    author_email=$(git -C "$FORGE_DIR" log -1 --format="%ae" "$sha")
    author_date=$(git -C "$FORGE_DIR" log -1 --format="%aI" "$sha")

    echo "[$N/$TOTAL] forge@${sha:0:8}: $msg"

    # Advance the sparse working tree to this commit
    git -C "$FORGE_DIR" checkout -q "$sha"

    apply_snapshot

    echo "$sha" > "$SHA_FILE"
    git add "$DEST_DIR/" "$SHA_FILE"

    GIT_AUTHOR_NAME="$author_name" \
    GIT_AUTHOR_EMAIL="$author_email" \
    GIT_AUTHOR_DATE="$author_date" \
    GIT_COMMITTER_DATE="$author_date" \
    git commit -m "forge@${sha:0:8}: $msg"
done

# Leave the upstream clone on master for the next run
git -C "$FORGE_DIR" checkout master --quiet 2>/dev/null || true

echo "Sync complete. Applied $N commit(s)."
