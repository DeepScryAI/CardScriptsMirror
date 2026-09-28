#!/usr/bin/env bash
#
# nightly_sync.sh -- regenerate the anonymous numeric corpus from upstream Forge.
#
# Replaces the retired raw-lineage `sync-cardsfolder.sh` (tag
# archive/main-wotc-20260819), which copied TITLED Forge scripts into this
# repository and stopped running when `main` became the anonymous trie.
# Nothing titled is committed here: the upstream clone lives outside the
# repository and only the generator's output is staged.
#
# Steps:
#   1. Fetch upstream Forge (sparse: cardsfolder + tokenscripts) into FORGE_DIR.
#   2. Refresh the Scryfall snapshot the generator and the IP scan share.
#   3. Run scripts/generate_uuid_trie.rs in INCREMENTAL mode (--token-ledger):
#      every existing ID is kept, changed scripts are regenerated, an ID that
#      cannot be regenerated is carried over unchanged, and new cards/tokens
#      without a catalog ID are reported, not generated.
#   4. IP gate (fail-closed, per file): any cards/ or tokens/ file whose
#      regenerated text matches a Scryfall title or Oracle pattern that its
#      previous version did not is restored to that previous version (or
#      dropped, if new). A rescan must then find no hit outside the previous
#      tip's hit set, or the run aborts without committing.
#   5. Commit (no push) with the upstream SHA recorded in .forge-upstream-sha.
#      The caller pushes: the workflow with GITHUB_TOKEN, or a person locally.
#
# Environment (all optional):
#   FORGE_DIR        upstream clone location   (default: /tmp/forge-upstream)
#   UPSTREAM_REPO    upstream URL              (default: Card-Forge/forge)
#   UPSTREAM_BRANCH  upstream branch           (default: master)
#   SCRYFALL_REFRESH 1 to force a new snapshot (default: 1)
#   NO_COMMIT        1 to stop before committing (dry run)
#
# Exit status: 0 with or without a new commit; non-zero on any failure,
# including an IP-gate violation.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

UPSTREAM_REPO="${UPSTREAM_REPO:-https://github.com/Card-Forge/forge.git}"
UPSTREAM_BRANCH="${UPSTREAM_BRANCH:-master}"
FORGE_DIR="${FORGE_DIR:-/tmp/forge-upstream}"
SCRYFALL_REFRESH="${SCRYFALL_REFRESH:-1}"
NO_COMMIT="${NO_COMMIT:-0}"
SCRYFALL_CACHE="$REPO_ROOT/.cache/scryfall/default_cards.json"
REPORTS="$REPO_ROOT/.cache/reports"
mkdir -p "$REPORTS"

if [ -n "$(git status --porcelain -- cards tokens token_ids.tsv .forge-upstream-sha)" ]; then
  echo "nightly_sync: refusing to run over uncommitted corpus changes" >&2
  exit 1
fi

# ---- 1. upstream Forge ------------------------------------------------------
if [ ! -d "$FORGE_DIR/.git" ]; then
  git clone --quiet --filter=blob:none --no-checkout --depth 1 --branch "$UPSTREAM_BRANCH" \
    "$UPSTREAM_REPO" "$FORGE_DIR"
  git -C "$FORGE_DIR" sparse-checkout set --no-cone forge-gui/res/cardsfolder forge-gui/res/tokenscripts
fi
git -C "$FORGE_DIR" fetch --quiet --depth 1 origin "$UPSTREAM_BRANCH"
git -C "$FORGE_DIR" checkout --quiet --force FETCH_HEAD
UPSTREAM_SHA="$(git -C "$FORGE_DIR" rev-parse HEAD)"
UPSTREAM_DATE="$(git -C "$FORGE_DIR" log -1 --format=%cI HEAD)"
echo "nightly_sync: upstream $UPSTREAM_SHA ($UPSTREAM_DATE)"
RES="$FORGE_DIR/forge-gui/res"

# ---- 2 + 3. regenerate --------------------------------------------------------
REFRESH_FLAG=()
if [ "$SCRYFALL_REFRESH" = "1" ]; then REFRESH_FLAG=(--refresh); fi
./scripts/generate_uuid_trie.rs \
  --source "$RES/cardsfolder" \
  --token-source "$RES/tokenscripts" \
  --token-ledger token_ids.tsv \
  --catalog catalog_ids.tsv \
  --cache "$SCRYFALL_CACHE" \
  --output cards \
  --token-output tokens \
  "${REFRESH_FLAG[@]}"
cp "$REPORTS/generate-report.json" "$REPORTS/nightly-generate-report.json"

# ---- 4. IP gate ------------------------------------------------------------
scan() { # $1 = label; scans the WORKING TREE's tracked + staged corpus files
  local label="$1" prefix
  for prefix in cards tokens; do
    ./scripts/scan_scryfall_ip.rs --root . --path-prefix "$prefix" --cache "$SCRYFALL_CACHE" \
      --report "$REPORTS/ip-$label-$prefix.json" >/dev/null 2>&1 || true
  done
}
git add -A -- cards tokens
# Baseline: the previous tip's corpus, scanned from a throwaway worktree.
BASE_DIR="$(mktemp -d)"
git worktree add --quiet --detach "$BASE_DIR/base" HEAD
( cd "$BASE_DIR/base" && for prefix in cards tokens; do
    "$REPO_ROOT/scripts/scan_scryfall_ip.rs" --root . --path-prefix "$prefix" --cache "$SCRYFALL_CACHE" \
      --allowlist "$REPO_ROOT/ip_allowlist.tsv" --report "$REPORTS/ip-base-$prefix.json" >/dev/null 2>&1 || true
  done )
git worktree remove --force "$BASE_DIR/base"
rmdir "$BASE_DIR"
scan new
python3 - "$REPORTS" <<'PY'
import json, subprocess, sys
reports = sys.argv[1]
def hit_paths(label):
    paths = set()
    for prefix in ("cards", "tokens"):
        with open(f"{reports}/ip-{label}-{prefix}.json") as f:
            report = json.load(f)
        if report.get("omitted_hit_pairs"):
            sys.exit(f"IP gate: {label}/{prefix} report truncated; cannot compare hit sets")
        paths |= {hit["path"] for hit in report["hits"]}
    return paths
base, new = hit_paths("base"), hit_paths("new")
restored = sorted(new - base)
for path in restored:
    known = subprocess.run(["git", "cat-file", "-e", f"HEAD:{path}"]).returncode == 0
    if known:
        subprocess.run(["git", "checkout", "HEAD", "--", path], check=True)
    else:
        subprocess.run(["git", "rm", "--quiet", "--cached", "--force", "--", path], check=True)
        subprocess.run(["rm", "-f", "--", path], check=True)
    print(f"IP gate: {'restored previous version of' if known else 'dropped new'} {path}")
with open(f"{reports}/nightly-ip-restored.json", "w") as f:
    json.dump(restored, f, indent=1)
PY
scan final
python3 - "$REPORTS" <<'PY'
import json, sys
reports = sys.argv[1]
def hit_paths(label):
    paths = set()
    for prefix in ("cards", "tokens"):
        with open(f"{reports}/ip-{label}-{prefix}.json") as f:
            paths |= {hit["path"] for hit in json.load(f)["hits"]}
    return paths
extra = sorted(hit_paths("final") - hit_paths("base"))
if extra:
    sys.exit(f"IP gate: {len(extra)} new hit paths survived restoration: {extra[:10]}")
print("IP gate: no hit outside the previous tip's hit set")
PY

# ---- 5. commit -------------------------------------------------------------
printf '%s\n' "$UPSTREAM_SHA" > .forge-upstream-sha
git add -A -- cards tokens .forge-upstream-sha
SUMMARY="$(python3 - "$REPORTS" <<'PY'
import json, sys
r = json.load(open(f"{sys.argv[1]}/nightly-generate-report.json"))
restored = json.load(open(f"{sys.argv[1]}/nightly-ip-restored.json"))
print(f"generated {r['generated_scripts']} card scripts from {r['source_scripts']} sources; "
      f"{len(r['missing_mappings'])} without a catalog ID; "
      f"{len(r['unmapped_tokens'])} tokens and {len(r['unmapped_token_references'])} cards awaiting a token ID; "
      f"{len(r['carried_over_cards'])} cards and {len(r['carried_over_tokens'])} tokens carried over unchanged; "
      f"{len(restored)} files held back by the IP gate")
PY
)"
echo "nightly_sync: $SUMMARY"
if git diff --cached --quiet; then
  echo "nightly_sync: corpus already matches upstream $UPSTREAM_SHA; nothing to commit"
  exit 0
fi
git diff --cached --shortstat
if [ "$NO_COMMIT" = "1" ]; then
  echo "nightly_sync: NO_COMMIT=1, leaving the regenerated corpus staged"
  exit 0
fi
git commit --quiet -m "sync: regenerate from upstream Forge ${UPSTREAM_SHA:0:10} (${UPSTREAM_DATE%%T*})" \
  -m "$SUMMARY" -m "Upstream: $UPSTREAM_REPO@$UPSTREAM_SHA"
git log --oneline -1
