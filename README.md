# CardScriptsMirror anonymous numeric pipeline

This unrelated-history branch generates an identity-only card script corpus.
It joins Forge scripts to stable DeepScry numeric IDs through Scryfall
`oracle_id` values, removes top-level and variant display metadata (`Name:` and
`Oracle:`), removes every human description/prompt parameter, and writes the
executable scripts beneath a three-level numeric trie.

The generated `cards/` tree is the replacement gameplay corpus on this branch.
The downloaded Scryfall snapshot remains untracked and is used only as an
offline build input. DeepScry's numeric loader consumes the generated tree
without consulting that snapshot or a card-title registry.

## Generate

Install `rust-script` 0.36 or newer, check out the archived source branch in a
sibling worktree, extract the title-free identity bridge from DeepScry's
append-only catalog, then run:

```sh
./scripts/extract_catalog_ids.rs \
  --source /path/to/DeepScry/src/engine/assets/card_catalog.tsv \
  --output catalog_ids.tsv

./scripts/generate_uuid_trie.rs \
  --source ../cardsmirror-source/cardsfolder \
  --catalog catalog_ids.tsv \
  --output cards
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

## Identity and layout

One Forge rules script represents one stable numeric catalog identity shared
by many printings. ID `12345678` is stored at:

```text
cards/12/34/56/12345678.txt
```

The first line is `Id:12345678`. No title appears in the path or in top-level
script fields. Structured fields such as `Name$` remain temporarily because
they can be executable references to other cards or effects; phase 3 migrates
those through typed numeric references rather than unsafe text substitution.

## Current compatibility limit

DeepScry must accept `Id:` as the definition identity, load files by numeric
path, and provide any title/rules/flavor presentation through a separate,
optional skin. The executable DSL must remain fully functional when that skin
is absent.
