# CardScriptsMirror

A read-only **mirror of the card-scripts folder** from the upstream
[Card-Forge/forge](https://github.com/Card-Forge/forge) project.

It tracks just one upstream directory — `forge-gui/res/cardsfolder/` — and
republishes it here, nested one level under [`./cardsfolder/`](./cardsfolder/).
That folder holds the tens of thousands of per-card script files in
letter-subfolders (`a/`, `b/`, `c/`, …, plus `rebalanced/` and `upcoming/`).

This mirror exists so downstream tooling can depend on **only** the card
scripts without cloning the entire (large) Forge repository.

## Layout

```
./cardsfolder/            <- the mirrored tree
    a/ b/ c/ ... z/        (per-card .txt script files, grouped by first letter)
    rebalanced/
    upcoming/
./.forge-upstream-sha     <- last replayed upstream forge commit (sync state)
./scripts/sync-cardsfolder.sh   <- the history-preserving sync logic (also used by CI)
./.github/workflows/sync.yml    <- scheduled + manual "tail upstream" workflow
./LICENSE  ./NOTICE        <- GPL-3.0 license + attribution (see below)
```

Upstream `forge-gui/res/cardsfolder/<letters>` maps to mirror
`./cardsfolder/<letters>`. The extra `cardsfolder/` nesting is deliberate
(requirement): the repo root is NOT a pile of naked `a/ b/ c/` folders.

## How it stays up to date

A GitHub Actions workflow ([`.github/workflows/sync.yml`](./.github/workflows/sync.yml))
"tails" upstream `master`:

- **Schedule:** every 6 hours (`cron: "0 */6 * * *"`).
- **Manual:** `workflow_dispatch` (Run workflow button), with an optional
  `from_sha` input to replay upstream commits from a specific starting SHA
  (used for catch-up testing or force-push recovery).

On each run it advances `./cardsfolder/` to upstream's latest state **one
upstream commit at a time** (see below), then pushes **only if new commits
were produced**. It pushes to this same repo using the built-in
`GITHUB_TOKEN` (the workflow grants itself `contents: write`) — no personal
access token or secret is required.

## How history is preserved

This mirror **preserves upstream commits**: every Card-Forge/forge commit that
touches `forge-gui/res/cardsfolder/` is replayed as its own commit in this
repo, keeping the original **author name, e-mail, and date**. So
`git log -- cardsfolder` here shows one commit per upstream cardsfolder commit
— the same per-commit attribution you would get from a `git subtree` merge.

### Why we replay instead of running `git subtree split`

The goal (the user's request) is a `git subtree`-style merge that keeps
per-upstream-commit history at `./cardsfolder/`. The textbook recipe is
`git subtree split --prefix=forge-gui/res/cardsfolder` on a Forge clone, then
subtree-merge the result. We achieve the **same outcome** with a lighter,
CI-friendly mechanism, because a literal `git subtree split` does not scale
here:

- Forge has **70k+ commits**. `git subtree split` rewrites the *entire*
  matching history on every run; measured locally it **does not finish within
  several minutes** even with a warm clone — it is far worse on a cold CI
  runner that must fault in historical blobs on demand.
- `subtree split` is stateful and fragile across upstream force-pushes.

Instead, `scripts/sync-cardsfolder.sh`:

1. Keeps a **partial (`blob:none`) + sparse** clone of Forge in `/tmp` (cached
   between CI runs via `actions/cache`). Partial clone keeps the **full commit
   graph** — enough to `git log` and replay the path — while deferring blob
   downloads, so only the trees we actually check out are fetched.
2. Lists upstream commits in `(<last-synced-sha> .. master]` that touched the
   path, oldest first.
3. For each, checks out that upstream commit, `rsync --delete`s the folder into
   `./cardsfolder/`, and makes a mirror commit **with the upstream author/date
   preserved**.
4. Records the last replayed upstream SHA in `./.forge-upstream-sha`, so the
   next run resumes incrementally (only NEW commits are replayed — never the
   whole history again).

A per-run cap (`MAX_COMMITS`, default 400) drains a large backlog over several
runs so a single CI job never times out.

> The initial/baseline commit is a single flattened snapshot (history before
> the baseline is not reconstructed — explicitly acceptable per the project
> brief). Everything **after** the baseline is replayed commit-by-commit.

### Residual risk: upstream force-push / history rewrite

If Card-Forge ever force-pushes or rebases `master` such that our stored
`.forge-upstream-sha` is no longer present in (or no longer an ancestor of)
upstream `master`, a clean `(last..HEAD]` range can't be computed. The sync
script **detects this** (`cat-file -e` + `merge-base --is-ancestor`) and
**recovers gracefully** by re-baselining: it imports the current upstream HEAD
as one fresh flattened snapshot commit, then resumes per-commit replay from
there. The trade-off is that the specific rewritten commits are collapsed into
that one re-baseline commit — the mirror stays correct and never corrupts, but
that single span loses per-commit attribution. This is the inherent cost of
mirroring a history that upstream rewrote, and it is the main downside of the
history-preserving approach versus the earlier stateless snapshot approach.

## Running the sync locally

```sh
# Replay all new upstream commits since the stored .forge-upstream-sha:
scripts/sync-cardsfolder.sh

# Replay starting after a specific upstream SHA (catch-up / recovery):
FROM_SHA=<upstream-sha> scripts/sync-cardsfolder.sh

# The script makes the mirror commits itself (author/date preserved); just push:
git push
```

## Provenance

`./.forge-upstream-sha` records the most recent upstream `Card-Forge/forge`
commit replayed into this mirror. Every mirrored commit's message also carries
the exact upstream SHA it came from (`forge@<short>: <subject>` plus a
`Mirrored from Card-Forge/forge commit <full-sha>.` body line), and the
upstream author/date are preserved on the commit itself.

This mirror is derivative of Card-Forge/forge; the card-script content
retains its upstream license/ownership. See the upstream repository for
authorship and history.

## License & Attribution

This repository is an **unofficial mirror** and is **not affiliated with,
endorsed by, or maintained by** the Forge project.

The contents of [`cardsfolder/`](./cardsfolder/) are mirrored **verbatim**
from [Card-Forge/forge](https://github.com/Card-Forge/forge), specifically
`forge-gui/res/cardsfolder/`. Upstream Forge is licensed under the **GNU
General Public License, version 3.0 (GPL-3.0)**, and the `cardsfolder/`
directory carries no separate notice, so it is covered by that GPL-3.0
license. This repository therefore **redistributes the card scripts under the
same GPL-3.0 license**.

- Full license text: [`LICENSE`](./LICENSE) (a verbatim copy of upstream
  Forge's `LICENSE`).
- Attribution details: [`NOTICE`](./NOTICE).
- Source commit provenance: `./.forge-upstream-sha` (and each sync commit
  message references the upstream commit it mirrors).

`SPDX-License-Identifier: GPL-3.0-only`
