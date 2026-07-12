# READ-ONLY MIRROR — do NOT edit card scripts here

This repository (`DeepScryAI/CardScriptsMirror`) is a **faithful, read-only
mirror** of upstream Forge's card-script data (`res/cardsfolder/` and the sibling
`tokenscripts/`, `puzzle/`, `tutorial/`, `editions/` resources) from
[`Card-Forge/forge`](https://github.com/Card-Forge/forge). DeepScry consumes it
as a git submodule (`cardsfolder-mirror`). It exists so DeepScry stays in lockstep
with upstream card data; its value is entirely that it matches upstream exactly.

## The rule

**Do NOT hand-edit card scripts (or any content) in this repository.** Editing a
card here silently diverges DeepScry from upstream Forge: the next re-sync from
upstream either clobbers your edit or, worse, creates a permanent hidden fork that
no one can reconcile. A local "fix" here looks like it works but is not real — it
is not in upstream, and it hides the actual defect.

## When a card behaves wrong, do THIS instead

1. **Assume it is OUR ENGINE first (it usually is).** The overwhelmingly common
   cause of wrong card behavior in DeepScry is that the DeepScry engine does not
   yet correctly interpret an upstream script construct — not that the script is
   wrong. Fix the bug in the **DeepScry engine** (the main `DeepScry` repo), and
   add a test/puzzle there. Do not touch this mirror.

2. **Only if the UPSTREAM script itself is genuinely wrong** (a real bug in
   Forge's data, verified against the card's Oracle text and the MTG
   Comprehensive Rules): open a pull request **upstream** at
   [`Card-Forge/forge`](https://github.com/Card-Forge/forge). Once it merges
   upstream, **re-sync this mirror** from upstream. Never hand-edit the mirror as
   a shortcut — that defeats the entire point of a faithful mirror.

## Why this matters

A faithful mirror is a trustworthy baseline: when a card misbehaves we can be sure
the script matches upstream, so the bug is in our engine. Every local edit here
erodes that guarantee and turns "does our engine handle upstream's data correctly?"
into an unanswerable question. Keep the mirror pristine.

(A codex agent once edited a card here to "fix" Sylvan Library — that edit was
wrong precisely because it hid an engine gap and forked us from upstream. This
guard exists so it never happens again.)

Re-sync tooling lives under `scripts/`; the upstream commit this mirror tracks is
recorded in `.forge-upstream-sha`.
