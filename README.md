# CardScriptsMirror anonymous numeric pipeline

This unrelated-history branch generates an identity-only card script corpus.
It joins Forge scripts to stable DeepScry numeric IDs through Scryfall
`oracle_id` values, removes top-level and variant display metadata (`Name:` and
`Oracle:`), removes every human description/prompt parameter, and writes the
executable scripts beneath a three-level numeric trie.

The generated `cards/` and `tokens/` trees are the replacement gameplay corpus on this branch.
The downloaded Scryfall snapshot remains untracked and is used only as an
offline build input. DeepScry's numeric loader consumes the generated tree
without consulting that snapshot or a card-title registry.

Each script's structured `ColorIdentity:` is generated once from Scryfall's
metadata and explicit non-reminder mana symbols. Basic-land-type colors are
kept out of that CR 903.4 field because Commander applies them as the separate
CR 903.5d deck restriction. Runtime code therefore needs neither Oracle text
nor Scryfall's broader combined identity field.

## Generate

Install `rust-script` 0.36 or newer, check out the archived source branch in a
sibling worktree, extract the title-free identity bridge from DeepScry's
append-only catalog, then run:

```sh
./scripts/extract_catalog_ids.rs \
  --source /path/to/DeepScry/src/engine/assets/card_catalog.tsv \
  --output catalog_ids.tsv \
  --face-output catalog_face_ids.tsv

./scripts/generate_uuid_trie.rs \
  --source ../cardsmirror-source/cardsfolder \
  --catalog catalog_ids.tsv \
  --token-catalog /path/to/DeepScry/src/core/assets/token_catalog.tsv \
  --output cards \
  --token-output tokens
```

The first run downloads Scryfall's `default_cards` bulk snapshot into
`.cache/scryfall/`; later runs reuse it unless `--refresh` is supplied. A
machine-readable report is written under `.cache/reports/`. Missing and
ambiguous mappings are always printed and recorded. Add `--strict` when those
expected Forge-only records should fail the run.

Run the unit tests with:

```sh
rust-script --test scripts/generate_uuid_trie.rs
rust-script --test scripts/extract_catalog_ids.rs
rust-script --test scripts/scan_scryfall_ip.rs
```

`catalog_ids.tsv` contains a numeric ID, an `oracle_id`, and a one-way SHA-256
digest used to distinguish multiple Scryfall names for the same Oracle
identity. It carries no plaintext titles or card text. Scryfall data is used
only while generating the corpus and is never shipped as gameplay data.

`catalog_face_ids.tsv` is the companion index for SINGLE-FACE spellings. A
two-faced card's registry name is the combined form ("A // B"), so a caller
holding only one face's name cannot match the combined digest. Each row is the
SHA-256 of one face spelling and the numeric ID of the card that owns it, or
the literal `ambiguous` where two different cards share a face spelling. The
generator derives it with the same rules DeepScry's own catalog uses: a full
card name owns its spelling, so a face that is also some card's full name is
left out entirely, and a face two cards share is recorded as ambiguous and
refused rather than resolved to one of them. Like the identity table it is
digest-only, and like the identity table it is NOT packed into the cardset
tarball (`pack_cardset.rs` packs `manifest.json` plus `cards/` and `tokens/`
and nothing else), so publishing it changes no cardset content id.

These identity fields are redistribution protections, not secrecy mechanisms.
The retained `oracle_id` is Scryfall's stable public identifier and an
intentional public join key. `name_sha256` avoids redistributing a title while
letting the generator distinguish otherwise ambiguous public records; anyone
who already has the public title dataset can recompute it. Likewise, the
truncated SHA-256 `set_group`/`OriginSet` value avoids shipping a set code, but
the input space is only a few hundred public values and is trivially
rainbow-tableable. None of these values is intended to conceal which public
record produced a script; the protection is that the executable corpus
(`cards/` and `tokens/`) redistributes no title, Oracle text, or other
protected expression. Card titles do appear, deliberately, in the separate
presentation catalog (`presentation/title_catalog.tsv`, an optional
title-only skin — see `presentation/README.md`); they are kept out of the
executable corpus itself.

## Identity and layout

One Forge rules script represents one stable numeric catalog identity shared
by many printings. ID `12345678` is stored at:

```text
cards/12/34/56/12345678.txt
```

The first line is `Id:12345678`. No title appears in the path or in top-level
script fields. Executable references to catalog cards, synthetic objects, and
token definitions are rewritten into one shared numeric catalog namespace;
dynamic selectors such as `ChosenName` remain vocabulary rather than content.

Token scripts use the same trie layout under `tokens/`, begin with `Id:`, and
are addressed from card DSL records through `TokenScriptIds$`. The one-time
migration preserves card IDs 1–35307 and appends the frozen 837 token ledger
rows as IDs 35308–36144. Future definitions append after that shared range;
no identity is derived from a filename or content hash.

## Current compatibility limit

DeepScry must accept `Id:` as the definition identity, load files by numeric
path, and provide any title/rules/flavor presentation through a separate,
optional skin. The executable DSL must remain fully functional when that skin
is absent.

## Single-threaded development policy

Owner ruling, 2026-08-22: this repository is used only by the DeepScry
workstream and is developed **single-threaded, landing to `main` with no
side tracks**.

- Commit directly to `main`, in small commits; history stays linear (plain
  fast-forward only, never merge commits, never force-push).
- Do **not** create side branches. Work that is not ready to land on `main`
  stays local.
- Every commit that an external repository pins (a DeepScry submodule
  gitlink) is anchored by an annotated tag under `pin/` so the pinned commit
  stays fetchable regardless of how `main` moves.
- Retired lineages are preserved as `archive/` tags, never as branches.
