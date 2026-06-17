#!/usr/bin/env bash
#
# sync-cardsfolder.sh
#
# Mirror a set of resource folders from Card-Forge/forge's forge-gui/res/ into
# this repo as SIBLING directories at the repo root, PRESERVING upstream
# history: every upstream commit that touches ANY of the mirrored paths is
# replayed as a commit in THIS repo, keeping the original author name/email/date.
# The mirror's `git log` therefore shows one commit per upstream commit that
# touched the mirrored set (same outcome a `git subtree` merge would give — see
# README "How history is preserved" for why we replay instead of running
# `git subtree split`).
#
# Mirrored paths (siblings at the repo root):
#   forge-gui/res/cardsfolder/   -> ./cardsfolder/    (tens of thousands of card scripts)
#   forge-gui/res/tokenscripts/  -> ./tokenscripts/   (~800 token-creation scripts)
#   forge-gui/res/puzzle/        -> ./puzzle/         (~363 puzzle .pzl fixtures)
#   forge-gui/res/tutorial/      -> ./tutorial/       (1 tutorial .pzl)
#
# A downstream consumer that resolves `cardsfolder/../<name>` finds each of the
# others because they live side by side at the same root.
#
# Behaviour
# ---------
#   * First run (no .forge-upstream-sha):
#       Flat snapshot import of the current upstream master HEAD for ALL paths,
#       recording the upstream SHA. (History before the baseline is intentionally
#       flattened, per the project's "initial commit may flatten" allowance.)
#   * Subsequent runs:
#       Replay every upstream commit in (.forge-upstream-sha .. master] that
#       touched ANY mirrored path, in chronological order, as individual mirror
#       commits. Each replayed commit re-snapshots ALL paths, so a commit that
#       only touched tokenscripts still leaves cardsfolder byte-identical.
#
# Idempotency
# -----------
#   The snapshot of each path is a plain `rsync -a --delete` from a single
#   explicit upstream source dir into a single explicit destination dir. Sources
#   are an EXPLICIT enumerated list (see MIRROR_PATHS) — never a recursive walk
#   of the repo — so a re-run converges and cannot nest a dir inside itself or
#   duplicate content. Re-running with no upstream change is a no-op.
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
#   UPSTREAM_REPO/UPSTREAM_RES/FORGE_DIR – advanced overrides.
#
# Idea credit: the per-commit replay design (preserve author/date, MAX_COMMITS
# cap, FROM_SHA override, persistent /tmp upstream clone) is adapted from the
# Copilot-authored PR #1 on this repo; hardened here with force-push detection
# + recovery and an ancestry check on the stored SHA.

set -euo pipefail

# ---- Config -----------------------------------------------------------------
UPSTREAM_REPO="${UPSTREAM_REPO:-https://github.com/Card-Forge/forge.git}"
# Parent directory in upstream that holds every mirrored folder.
UPSTREAM_RES="${UPSTREAM_RES:-forge-gui/res}"
SHA_FILE="${SHA_FILE:-.forge-upstream-sha}"
FORGE_DIR="${FORGE_DIR:-/tmp/forge-upstream}"
MAX_COMMITS="${MAX_COMMITS:-400}"
UPSTREAM_BRANCH="${UPSTREAM_BRANCH:-master}"

# EXPLICIT enumerated list of mirrored folder names. Each NAME is mirrored from
# upstream "$UPSTREAM_RES/NAME" into "./NAME" — sibling dirs at the repo root.
# To mirror another sibling resource folder, add its name here (one place).
MIRROR_PATHS=(cardsfolder tokenscripts puzzle tutorial editions)

# Upstream sub-paths (relative to repo root inside the upstream clone) used for
# both sparse-checkout and the history-filtering `git log -- <paths>`.
UPSTREAM_PATHS=()
for name in "${MIRROR_PATHS[@]}"; do
    UPSTREAM_PATHS+=("$UPSTREAM_RES/$name")
done

# Always operate from the mirror repo root (parent of this script's dir).
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"
[ -d .git ] || { echo "ERROR: not a git repo root: $REPO_DIR" >&2; exit 1; }

# ---- 1. Set up / refresh the upstream clone ---------------------------------
# IMPORTANT: this is a partial (blob:none) + sparse clone. Partial clone keeps
# FULL commit history (so we can `git log` the paths and replay commits) while
# deferring blob download — only the blobs we actually check out are fetched.
# That is what makes replay cheap, where a full `git subtree split` is not.
if [ -d "$FORGE_DIR/.git" ]; then
    echo "==> Refreshing cached upstream clone at $FORGE_DIR"
    git -C "$FORGE_DIR" fetch --filter=blob:none --quiet origin "$UPSTREAM_BRANCH"
    git -C "$FORGE_DIR" checkout --quiet -B "$UPSTREAM_BRANCH" FETCH_HEAD
    git -C "$FORGE_DIR" reset --hard --quiet FETCH_HEAD
    # Ensure the sparse set covers every mirrored path (a cache made by an
    # older single-path version of this script would only have cardsfolder).
    git -C "$FORGE_DIR" sparse-checkout set "${UPSTREAM_PATHS[@]}"
else
    echo "==> Cloning upstream $UPSTREAM_REPO (partial+sparse: ${UPSTREAM_PATHS[*]})"
    git clone --filter=blob:none --no-checkout --quiet "$UPSTREAM_REPO" "$FORGE_DIR"
    git -C "$FORGE_DIR" sparse-checkout init --cone
    git -C "$FORGE_DIR" sparse-checkout set "${UPSTREAM_PATHS[@]}"
    git -C "$FORGE_DIR" checkout --quiet "$UPSTREAM_BRANCH"
fi
UPSTREAM_HEAD="$(git -C "$FORGE_DIR" rev-parse HEAD)"

# ---- Helpers ----------------------------------------------------------------
apply_snapshot() {
    # Make every mirrored ./NAME/ byte-identical to upstream's
    # "$UPSTREAM_RES/NAME" at the current checked-out upstream commit.
    # --delete propagates upstream deletions. Sources are the EXPLICIT enumerated
    # list, so this never recurses into or nests a dir within itself.
    local name
    for name in "${MIRROR_PATHS[@]}"; do
        mkdir -p "$name"
        rsync -a --delete "$FORGE_DIR/$UPSTREAM_RES/$name/" "$name/"
    done
}

stage_snapshot() {  # stage all mirrored dirs + the SHA file
    git add -A "${MIRROR_PATHS[@]}" "$SHA_FILE"
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
# Also used to back-fill: if the stored SHA is fine but a newly-added mirrored
# path is missing from the working tree (e.g. tokenscripts/puzzle/tutorial were
# just added to MIRROR_PATHS), re-snapshot at the current SHA to materialize
# them without losing the history pointer. apply_snapshot covers all paths, and
# the commit only records the genuinely-new content (empty diff => no commit).
if [ -z "$LAST_SHA" ]; then
    echo "==> Flat import of [${MIRROR_PATHS[*]}] at forge@${UPSTREAM_HEAD:0:8}"
    git -C "$FORGE_DIR" checkout --quiet "$UPSTREAM_HEAD"
    apply_snapshot
    echo "$UPSTREAM_HEAD" > "$SHA_FILE"
    stage_snapshot
    if git diff --staged --quiet; then
        echo "==> Nothing to import; mirror already matches forge@${UPSTREAM_HEAD:0:8}."
    else
        git commit -q -m "Import [${MIRROR_PATHS[*]}] snapshot from Card-Forge/forge@${UPSTREAM_HEAD:0:8}" \
            -m "Flattened baseline of ${UPSTREAM_RES}/{$(IFS=,; echo "${MIRROR_PATHS[*]}")}/ at upstream ${UPSTREAM_HEAD}."
        echo "==> Baseline import committed."
    fi
    exit 0
fi

# ---- 3b. Incremental replay (history-preserving) ----------------------------
mapfile -t COMMITS < <(
    git -C "$FORGE_DIR" log --reverse --format="%H" \
        "${LAST_SHA}..${UPSTREAM_HEAD}" -- "${UPSTREAM_PATHS[@]}"
)
TOTAL="${#COMMITS[@]}"

# Back-fill guard: even when there are no new upstream commits, a freshly-added
# mirrored path may be absent from the working tree. Re-snapshot at the stored
# SHA so the new sibling dirs get materialized + committed once.
if [ "$TOTAL" -eq 0 ]; then
    echo "==> No new upstream commits since ${LAST_SHA:0:8}; checking for missing mirrored paths."
    git -C "$FORGE_DIR" checkout --quiet "$LAST_SHA" 2>/dev/null \
        || git -C "$FORGE_DIR" checkout --quiet "$UPSTREAM_HEAD"
    apply_snapshot
    stage_snapshot
    if git diff --staged --quiet; then
        echo "==> Already up to date (last synced ${LAST_SHA:0:8}). Nothing to do."
        git -C "$FORGE_DIR" checkout -q "$UPSTREAM_BRANCH" 2>/dev/null || true
        exit 0
    fi
    echo "==> Materializing newly-added mirrored path(s) at forge@${LAST_SHA:0:8}."
    git commit -q -m "Back-fill mirrored paths [${MIRROR_PATHS[*]}] at forge@${LAST_SHA:0:8}" \
        -m "Added sibling resource folders to the mirror at upstream ${LAST_SHA}."
    git -C "$FORGE_DIR" checkout -q "$UPSTREAM_BRANCH" 2>/dev/null || true
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
    stage_snapshot

    if git diff --staged --quiet; then
        # Upstream commit touched a mirrored path per `log` but produced no net
        # change in our snapshot (e.g. a revert-pair collapsed). Skip empty commit.
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
