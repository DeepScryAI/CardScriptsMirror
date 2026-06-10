#!/usr/bin/env bash
#
# sync-cardsfolder.sh
#
# Mirror forge-gui/res/cardsfolder from Card-Forge/forge into ./cardsfolder/,
# PRESERVING upstream history: every upstream commit that touches the
# cardsfolder path is replayed as a commit in THIS repo, keeping the original
# author name/email/date. The mirror's `git log -- cardsfolder` therefore shows
# one commit per upstream cardsfolder commit (same outcome a `git subtree`
# merge would give — see README "How history is preserved" for why we replay
# instead of running `git subtree split`).
#
# Behaviour
# ---------
#   * First run (no .forge-upstream-sha):
#       Flat snapshot import of the current upstream master HEAD, recording the
#       upstream SHA. (History before the baseline is intentionally flattened,
#       per the project's "initial commit may flatten" allowance.)
#   * Subsequent runs:
#       Replay every upstream commit in (.forge-upstream-sha .. master] that
#       touched the path, in chronological order, as individual mirror commits.
#
# State / provenance
# ------------------
#   .forge-upstream-sha  – committed file recording the last replayed upstream
#                          SHA. Makes incremental syncs idempotent across runs.
#
# Environment variables (all optional)
# ------------------------------------
#   FROM_SHA     – Override the stored SHA; start replaying after this commit.
#                  Used for catch-up testing and for force-push recovery.
#   MAX_COMMITS  – Cap commits applied per run (default 400). A large backlog is
#                  drained over multiple runs so a single CI job never times out.
#   UPSTREAM_REPO/UPSTREAM_PATH/DEST_DIR/FORGE_DIR – advanced overrides.
#
# Idea credit: the per-commit replay design (preserve author/date, MAX_COMMITS
# cap, FROM_SHA override, persistent /tmp upstream clone) is adapted from the
# Copilot-authored PR #1 on this repo; hardened here with force-push detection
# + recovery and an ancestry check on the stored SHA.

set -euo pipefail

# ---- Config -----------------------------------------------------------------
UPSTREAM_REPO="${UPSTREAM_REPO:-https://github.com/Card-Forge/forge.git}"
UPSTREAM_PATH="${UPSTREAM_PATH:-forge-gui/res/cardsfolder}"
DEST_DIR="${DEST_DIR:-cardsfolder}"
SHA_FILE="${SHA_FILE:-.forge-upstream-sha}"
FORGE_DIR="${FORGE_DIR:-/tmp/forge-upstream}"
MAX_COMMITS="${MAX_COMMITS:-400}"
UPSTREAM_BRANCH="${UPSTREAM_BRANCH:-master}"

# Always operate from the mirror repo root (parent of this script's dir).
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"
[ -d .git ] || { echo "ERROR: not a git repo root: $REPO_DIR" >&2; exit 1; }

# ---- 1. Set up / refresh the upstream clone ---------------------------------
# IMPORTANT: this is a partial (blob:none) + sparse clone. Partial clone keeps
# FULL commit history (so we can `git log` the path and replay commits) while
# deferring blob download — only the blobs we actually check out are fetched.
# That is what makes replay cheap, where a full `git subtree split` is not.
if [ -d "$FORGE_DIR/.git" ]; then
    echo "==> Refreshing cached upstream clone at $FORGE_DIR"
    git -C "$FORGE_DIR" fetch --filter=blob:none --quiet origin "$UPSTREAM_BRANCH"
    git -C "$FORGE_DIR" checkout --quiet -B "$UPSTREAM_BRANCH" FETCH_HEAD
    git -C "$FORGE_DIR" reset --hard --quiet FETCH_HEAD
else
    echo "==> Cloning upstream $UPSTREAM_REPO (partial+sparse: $UPSTREAM_PATH)"
    git clone --filter=blob:none --no-checkout --quiet "$UPSTREAM_REPO" "$FORGE_DIR"
    git -C "$FORGE_DIR" sparse-checkout init --cone
    git -C "$FORGE_DIR" sparse-checkout set "$UPSTREAM_PATH"
    git -C "$FORGE_DIR" checkout --quiet "$UPSTREAM_BRANCH"
fi
UPSTREAM_HEAD="$(git -C "$FORGE_DIR" rev-parse HEAD)"

# ---- Helpers ----------------------------------------------------------------
apply_snapshot() {
    # Make ./cardsfolder/ byte-identical to upstream's path at the current
    # checked-out upstream commit. --delete propagates upstream deletions.
    mkdir -p "$DEST_DIR"
    rsync -a --delete "$FORGE_DIR/$UPSTREAM_PATH/" "$DEST_DIR/"
}

upstream_has_commit() {  # is $1 an object present in the upstream clone?
    git -C "$FORGE_DIR" cat-file -e "${1}^{commit}" 2>/dev/null
}
upstream_is_ancestor() { # is $1 an ancestor of upstream HEAD? (i.e. not rewritten away)
    git -C "$FORGE_DIR" merge-base --is-ancestor "$1" "$UPSTREAM_HEAD" 2>/dev/null
}

# ---- 2. Determine the sync starting point -----------------------------------
if [ -n "${FROM_SHA:-}" ]; then
    echo "==> FROM_SHA override: starting after ${FROM_SHA:0:8}"
    LAST_SHA="$FROM_SHA"
elif [ -f "$SHA_FILE" ]; then
    LAST_SHA="$(tr -d '[:space:]' < "$SHA_FILE")"
    echo "==> Continuing from last sync: ${LAST_SHA:0:8}"
else
    LAST_SHA=""
    echo "==> No $SHA_FILE found; performing initial flat import."
fi

# ---- 2b. Force-push / history-rewrite detection + recovery ------------------
# If the stored SHA is no longer present in upstream, or is no longer an
# ancestor of upstream HEAD, upstream rewrote/force-pushed its history. We can
# no longer compute a clean (LAST_SHA..HEAD] range. Recovery: re-baseline by
# importing the current HEAD as a fresh flat snapshot (a single catch-up commit
# that re-aligns the mirror), then continue preserving commits from there. This
# is the documented residual risk of the subtree/replay model — we degrade to
# one flatten commit rather than corrupting the mirror.
if [ -n "$LAST_SHA" ]; then
    if ! upstream_has_commit "$LAST_SHA"; then
        echo "WARNING: stored upstream SHA ${LAST_SHA:0:8} is GONE from upstream"
        echo "         (force-push / history rewrite). Re-baselining to HEAD."
        LAST_SHA=""
    elif ! upstream_is_ancestor "$LAST_SHA"; then
        echo "WARNING: stored upstream SHA ${LAST_SHA:0:8} is no longer an ancestor"
        echo "         of upstream HEAD (force-push / rebase). Re-baselining to HEAD."
        LAST_SHA=""
    fi
fi

# ---- 3a. Initial / re-baseline flat import ----------------------------------
if [ -z "$LAST_SHA" ]; then
    echo "==> Flat import of cardsfolder at forge@${UPSTREAM_HEAD:0:8}"
    git -C "$FORGE_DIR" checkout --quiet "$UPSTREAM_HEAD"
    apply_snapshot
    echo "$UPSTREAM_HEAD" > "$SHA_FILE"
    git add -A "$DEST_DIR" "$SHA_FILE"
    if git diff --staged --quiet; then
        echo "==> Nothing to import; mirror already matches forge@${UPSTREAM_HEAD:0:8}."
    else
        git commit -q -m "Import cardsfolder snapshot from Card-Forge/forge@${UPSTREAM_HEAD:0:8}" \
            -m "Flattened baseline of forge-gui/res/cardsfolder/ at upstream ${UPSTREAM_HEAD}."
        echo "==> Baseline import committed."
    fi
    exit 0
fi

# ---- 3b. Incremental replay (history-preserving) ----------------------------
mapfile -t COMMITS < <(
    git -C "$FORGE_DIR" log --reverse --format="%H" \
        "${LAST_SHA}..${UPSTREAM_HEAD}" -- "$UPSTREAM_PATH"
)
TOTAL="${#COMMITS[@]}"

if [ "$TOTAL" -eq 0 ]; then
    echo "==> Already up to date (last synced ${LAST_SHA:0:8}). Nothing to do."
    exit 0
fi
if [ "$TOTAL" -gt "$MAX_COMMITS" ]; then
    echo "==> $TOTAL commits pending; capping at $MAX_COMMITS this run (rest next run)."
    COMMITS=("${COMMITS[@]:0:$MAX_COMMITS}")
    TOTAL="$MAX_COMMITS"
fi

echo "==> Replaying $TOTAL upstream commit(s), preserving author/date..."
N=0
for sha in "${COMMITS[@]}"; do
    N=$((N + 1))
    msg=$(git -C "$FORGE_DIR" log -1 --format="%s" "$sha")
    a_name=$(git -C "$FORGE_DIR" log -1 --format="%an" "$sha")
    a_email=$(git -C "$FORGE_DIR" log -1 --format="%ae" "$sha")
    a_date=$(git -C "$FORGE_DIR" log -1 --format="%aI" "$sha")
    echo "[$N/$TOTAL] forge@${sha:0:8}: $msg"

    git -C "$FORGE_DIR" checkout -q "$sha"
    apply_snapshot
    echo "$sha" > "$SHA_FILE"
    git add -A "$DEST_DIR" "$SHA_FILE"

    if git diff --staged --quiet; then
        # Upstream commit touched the path per `log` but produced no net change
        # in our snapshot (e.g. a revert-pair collapsed). Skip empty commit.
        echo "        (no net change; skipping empty commit)"
        continue
    fi
    GIT_AUTHOR_NAME="$a_name" GIT_AUTHOR_EMAIL="$a_email" \
    GIT_AUTHOR_DATE="$a_date" GIT_COMMITTER_DATE="$a_date" \
    git commit -q -m "forge@${sha:0:8}: $msg" \
        -m "Mirrored from Card-Forge/forge commit ${sha}."
done

git -C "$FORGE_DIR" checkout -q "$UPSTREAM_BRANCH" 2>/dev/null || true
echo "==> Sync complete. Applied $N upstream commit(s)."
