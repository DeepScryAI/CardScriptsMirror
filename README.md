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
  --output catalog_ids.tsv

./scripts/rewrite_origin_sets.rs \
  --catalog catalog_ids.tsv \
  --cards cards

./scripts/generate_uuid_trie.rs \
  --source ../cardsmirror-source/cardsfolder \
  --catalog catalog_ids.tsv \
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

`catalog_ids.tsv` contains a numeric ID, an `oracle_id`, a one-way SHA-256
digest used to distinguish multiple Scryfall names for the same Oracle
identity, and a frozen anonymous origin-set ID such as `1993A` or `2025B`.
It carries no plaintext titles or card text. `set_ids.tsv` maps those anonymous
set IDs to full public Scryfall set UUIDs. Initial suffixes are assigned within
each year by release date and publisher set code; existing assignments never
move, so a set discovered later appends the next suffix even when its release
date is older. Suffixes continue `Z`, `AA`, `AB`, and so on. Scryfall data is
used only while generating the corpus and is never shipped as gameplay data.

These identity fields are redistribution protections, not secrecy mechanisms.
The retained `oracle_id` is Scryfall's stable public identifier and an
intentional public join key. `name_sha256` avoids redistributing a title while
letting the generator distinguish otherwise ambiguous public records; anyone
who already has the public title dataset can recompute it. The YEAR+LETTER
origin-set ID is a stable presentation label, while its full Scryfall UUID is
an intentional public join key. Neither is intended to conceal which public
record produced a script; the protection is that the repository redistributes
no title, Oracle text, publisher set code, or other protected expression.

## Identity and layout

One Forge rules script represents one stable numeric catalog identity shared
by many printings. ID `12345678` is stored at:

```text
cards/12/34/56/12345678.txt
```

The first line is `Id:12345678`. No title appears in the path or in top-level
script fields. Executable references to catalog cards, synthetic objects, and
token definitions are rewritten to their respective numeric namespaces;
dynamic selectors such as `ChosenName` remain vocabulary rather than content.

Token scripts use the same trie layout under `tokens/`, begin with `TokenId:`,
and are addressed from card DSL records through `TokenScriptIds$`. Token IDs
are deterministic SHA-256-derived nonzero integers in a namespace separate
from catalog card IDs.

## Current compatibility limit

DeepScry must accept `Id:` as the definition identity, load files by numeric
path, and provide any title/rules/flavor presentation through a separate,
optional skin. The executable DSL must remain fully functional when that skin
is absent.
