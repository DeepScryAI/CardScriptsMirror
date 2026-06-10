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
    .upstream-sha          (the upstream forge commit this snapshot was taken from)
./scripts/sync-cardsfolder.sh   <- the sync logic (also used by CI)
./.github/workflows/sync.yml    <- scheduled + manual "tail upstream" workflow
```

Upstream `forge-gui/res/cardsfolder/<letters>` maps to mirror
`./cardsfolder/<letters>`. The extra `cardsfolder/` nesting is deliberate
(requirement): the repo root is NOT a pile of naked `a/ b/ c/` folders.

## How it stays up to date

A GitHub Actions workflow ([`.github/workflows/sync.yml`](./.github/workflows/sync.yml))
"tails" upstream `master`:

- **Schedule:** every 6 hours (`cron: "0 */6 * * *"`).
- **Manual:** `workflow_dispatch` (Run workflow button), with an optional
  `upstream_ref` input to sync to a specific upstream commit/branch/tag.

On each run it sparse-clones upstream's `cardsfolder/`, rsyncs it into
`./cardsfolder/` (with `--delete`, so files removed upstream are removed
here too), and commits + pushes **only if there is a diff**. It pushes to
this same repo using the built-in `GITHUB_TOKEN` (the workflow grants itself
`contents: write`) — no personal access token or secret is required.

## Why folder-sync, not `git subtree`

The user suggested a `git subtree merge` of the single upstream folder. We
chose a **sparse-checkout + `rsync --delete` folder-sync** instead. Tradeoffs:

| | folder-sync (chosen) | git subtree |
|---|---|---|
| Per-upstream-commit attribution | No — flattened snapshots | Yes — preserves upstream commits |
| Robust to upstream force-pushes / history rewrites | **Yes** — each run is a fresh snapshot, no shared history to diverge | No — subtree's recorded splits break, merges conflict |
| Clone size / speed | Small — partial+sparse clone of one folder | Larger — needs enough upstream history to compute the subtree |
| Handles upstream deletions | **Yes** — `rsync --delete` | Yes |
| Implementation complexity | Low, stateless | Higher, stateful (remembers last split) |

Because this is an **experimental mirror and flattening history is
explicitly acceptable**, robustness and simplicity win. The folder-sync
treats every run as "make `./cardsfolder/` byte-identical to upstream's
current `forge-gui/res/cardsfolder/`," which is exactly what a mirror wants
and which cannot drift even if upstream rewrites its history.

### If you later want per-commit attribution (switch to subtree)

If down the line you want each mirror commit to map to an upstream commit,
switch to subtree roughly like this (one-time, then on a schedule):

```sh
# One-time graft of upstream's folder history under ./cardsfolder/
git remote add forge https://github.com/Card-Forge/forge.git
git fetch forge master
git read-tree --prefix=cardsfolder/ -u forge/master:forge-gui/res/cardsfolder
git commit -m "Graft upstream cardsfolder via subtree"

# On each update:
git fetch forge master
git merge -s subtree -Xsubtree=cardsfolder --squash forge/master
```

Note the subtree path is fragile if Card-Forge ever force-pushes `master`
or moves the folder; the folder-sync approach is immune to both.

## Running the sync locally

```sh
# Sync to upstream master (default):
scripts/sync-cardsfolder.sh

# Sync to a specific upstream commit/branch/tag:
scripts/sync-cardsfolder.sh <upstream-ref>

# Then review and commit:
git add -A cardsfolder
git commit -m "Sync cardsfolder to upstream <sha>"
```

## Provenance

`cardsfolder/.upstream-sha` records the exact upstream `Card-Forge/forge`
commit that the current snapshot was taken from. The sync commit messages
also reference that SHA.

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
- Source commit provenance: `cardsfolder/.upstream-sha` (and each sync
  commit message references the upstream commit it mirrors).

`SPDX-License-Identifier: GPL-3.0-only`
