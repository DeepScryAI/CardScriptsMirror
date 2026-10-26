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
#   4. Run card-compiler's pinned shared scanner and policy on cards/ and tokens/.
#      Any hit or scanner failure aborts before a commit. Expected WotC skins
#      and fetched upstream inputs are outside this executable corpus scope.
#   5. Re-mint presentation/skins/wotc.skin.json: pack the SS1 cardset from
#      cards/ + tokens/ and bind it to the unchanged presentation tables, keeping
#      each member's retrieval hints. Without this the manifest names the
#      previous cardset and every DeepScry cardset/network check refuses it.
#   6. Commit (no push) with the upstream SHA recorded in .forge-upstream-sha.
#      The caller pushes: the workflow with GITHUB_TOKEN, or a person locally.
#
# Environment:
#   FORGE_DIR        upstream clone location   (default: /tmp/forge-upstream)
#   UPSTREAM_REPO    upstream URL              (default: Card-Forge/forge)
#   UPSTREAM_BRANCH  upstream branch           (default: master)
#   SCRYFALL_REFRESH 1 to force a new snapshot (default: 1)
#   CARD_COMPILER_DIR independent checkout at .card-compiler-revision (required)
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

if [ -n "$(git status --porcelain -- cards tokens token_ids.tsv .forge-upstream-sha presentation)" ]; then
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
rust-script --force scripts/generate_uuid_trie.rs \
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
# Newly generated paths must be staged: the shared scanner uses git ls-files.
git add -A -- cards tokens
python3 scripts/check_corpus_ip.py --cache "$SCRYFALL_CACHE" --reports "$REPORTS"

# ---- 5. skin manifest ------------------------------------------------------
MANIFEST=presentation/skins/wotc.skin.json
CAS="$REPO_ROOT/.cache/cas"
mkdir -p "$CAS"
./scripts/pack_cardset.rs --cards cards --tokens tokens --output "$CAS/cardset.tar"
CATALOG_IDENTITY="$(head -1 presentation/title_catalog.tsv | tr ' ' '\n' | sed -n 's/^catalog_identity=//p')"
if [ "${#CATALOG_IDENTITY}" -ne 64 ]; then
  echo "nightly_sync: cannot read catalog_identity from presentation/title_catalog.tsv" >&2
  exit 1
fi
# Each presentation table's retrieval hint points at the commit that last
# changed that file: immutable, and it holds exactly these bytes, so the hint
# follows the table whenever a catalog generation changes it.
HINTS=()
for member_file in titles:title_catalog.tsv bodies:body_catalog.tsv \
                   artpack:artpack_scryfall_uuid.tsv provenance:provenance_oracle_ids.tsv; do
  member="${member_file%%:*}"; file="presentation/${member_file#*:}"
  if [ -n "$(git status --porcelain -- "$file")" ]; then
    echo "nightly_sync: $file has uncommitted changes; commit it before re-minting" >&2
    exit 1
  fi
  commit="$(git log -1 --format=%H -- "$file")"
  HINTS+=("--hint=$member=https://raw.githubusercontent.com/DeepScryAI/CardScriptsMirror/$commit/$file")
done
./scripts/make_skin_manifest.rs \
  --cardset "$CAS/cardset.tar" \
  --titles presentation/title_catalog.tsv \
  --bodies presentation/body_catalog.tsv \
  --artpack presentation/artpack_scryfall_uuid.tsv \
  --provenance presentation/provenance_oracle_ids.tsv \
  --catalog-identity "$CATALOG_IDENTITY" \
  "${HINTS[@]}" \
  --output "$MANIFEST"

# ---- 6. commit -------------------------------------------------------------
git add -A -- cards tokens "$MANIFEST"
# The upstream SHA is recorded only alongside a real corpus change: a commit
# carrying nothing but the moved SHA would advance main, and every advance of
# main costs DeepScry a full speculative-bump validate for nothing.
if git diff --cached --quiet -- cards tokens "$MANIFEST"; then
  echo "nightly_sync: upstream $UPSTREAM_SHA changes no generated script; nothing to commit"
  exit 0
fi
printf '%s\n' "$UPSTREAM_SHA" > .forge-upstream-sha
git add -- .forge-upstream-sha
SUMMARY="$(python3 - "$REPORTS" <<'PY'
import json, sys
r = json.load(open(f"{sys.argv[1]}/nightly-generate-report.json"))
print(f"generated {r['generated_scripts']} card scripts from {r['source_scripts']} sources; "
      f"{len(r['missing_mappings'])} without a catalog ID; "
      f"{len(r['unmapped_tokens'])} tokens and {len(r['unmapped_token_references'])} cards awaiting a token ID; "
      f"{len(r['carried_over_cards'])} cards and {len(r['carried_over_tokens'])} tokens carried over unchanged; "
      "shared IP gate passed for cards and tokens")
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
  -m "$SUMMARY" -m "Upstream: $UPSTREAM_REPO@$UPSTREAM_SHA" \
  -- cards tokens "$MANIFEST" .forge-upstream-sha
git log --oneline -1
