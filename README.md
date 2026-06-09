# CardScriptsMirror

A mirror of [`forge-gui/res/cardsfolder`](https://github.com/Card-Forge/forge/tree/master/forge-gui/res/cardsfolder)
from the [Card-Forge/forge](https://github.com/Card-Forge/forge) repository.

## Layout

```
cardsfolder/        ← mirror of forge-gui/res/cardsfolder (auto-updated)
.forge-upstream-sha ← last upstream Card-Forge/forge commit SHA that was synced
scripts/
  sync-cardsfolder.sh  ← sync script (run by CI, or manually)
.github/workflows/
  sync-cardsfolder.yml ← scheduled GitHub Actions workflow
```

## How it works

A GitHub Actions workflow runs every 6 hours and calls `scripts/sync-cardsfolder.sh`.

**First run** (no `.forge-upstream-sha` present):  
The script performs a flat import of the current `forge-gui/res/cardsfolder` tree
from the upstream `master` branch and records the upstream commit SHA in
`.forge-upstream-sha`.

**Subsequent runs** (incremental sync):  
The script fetches all upstream commits newer than the stored SHA that touch
`forge-gui/res/cardsfolder`, and replays them in order as mirrored commits —
preserving the original author name, e-mail, and date. Each mirrored commit
message is prefixed with `forge@<sha8>:`.

At most `MAX_COMMITS` (default 200) upstream commits are applied per run; if
more are pending, subsequent runs continue from where the last left off.

## Manual / catch-up sync

You can trigger the workflow manually from the **Actions** tab, optionally
supplying a `from_sha` to start replaying from a specific upstream commit.
This is useful for:

- Testing the catch-up logic against a range of real upstream commits.
- Recovering from a desync by rewinding to a known-good SHA.

```bash
# Run locally (requires git, rsync, bash)
FROM_SHA=<upstream-sha> bash scripts/sync-cardsfolder.sh
```

## Source repository

Card-Forge/forge: <https://github.com/Card-Forge/forge>  
Mirrored path: `forge-gui/res/cardsfolder`
