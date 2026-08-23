# Pipeline scripts

All executable tooling on this branch is safe Rust run through `rust-script`.
Each script embeds pinned Cargo dependency requirements and keeps downloaded or
diagnostic data below the gitignored `.cache/` directory.

`extract_catalog_ids.rs` removes plaintext presentation names from DeepScry's
append-only numeric catalog while retaining the numeric ID, Scryfall Oracle
UUID, and a one-way name digest needed to validate aliases. From one parse of
that catalog it writes two title-free tables: `catalog_ids.tsv` (one row per
card) and `catalog_face_ids.tsv` (one row per single-face spelling of a
multi-face card, so a caller holding only "A" can find the card whose registry
spelling is "A // B"). The face index reproduces the derivation rules in
DeepScry's `src/engine/src/card_catalog.rs`: a full card spelling owns its
name, and a face two different cards share is written as `ambiguous` and
refused rather than resolved to one of them. Neither table is packed into the
cardset tarball, so the face table changes no cardset content id.

`generate_uuid_trie.rs` owns Oracle-identity mapping, structured Forge-script
sanitization, and numeric-trie generation.

`extract_card_skin.rs` streams the same cached Scryfall snapshot and joins it
to `catalog_ids.tsv`, producing a numeric-ID keyed JSON table of presentation
titles and Oracle text. Its default output is
`.cache/card-skins/default.json`, below the repository's gitignored cache.
This generated table contains third-party expression and must never be added to
the mirror or to a consuming repository. A consumer may point `--output` at
another explicitly ignored local path.

`scan_scryfall_ip.rs` compiles all normalized Scryfall titles and Oracle texts
into an overlapping Aho-Corasick automaton and scans tracked repository text.
It shares the downloader/parser in `lib/scryfall_bulk.rs` with the generator.
By default it traverses submodules. Use `--exclude-submodules` only when the
audit intentionally treats independently versioned submodule repositories as
opaque gitlinks; the selected scope is recorded in the JSON report.

## Skin-format producers (the ratified SS0-SS5 package)

The card-skin format package ratified in DeepScry issue ds-5432 (normative
text: `docs/CARD_SKIN_FORMATS.md` in the DeepScry repository) is produced by
five scripts sharing `lib/cas.rs` — canonical JSON (RFC 8785), IPFS-compatible
content ids (CIDv1 / sha2-256 / raw / single block / base32), the
deterministic strict-ustar tarball writer, and `{cid, size, hints[]}` content
references. `lib/cas.rs` is pinned by known-answer tests to the same vectors
as DeepScry core's `src/core/src/cas/`; a change to either copy must update
both. Placement note: per the OD-3 ruling (card-compiler is the factory and
this repository becomes data-only) these producers and the shared lib are
slated to migrate into the card-compiler repository; they live here in the
interim so the pipeline stays runnable next to its data.

`pack_cardset.rs` packs `cards/` + `tokens/` plus a generated, deliberately
anonymous `manifest.json` into the deterministic cardset tarball
(`.cache/cas/cardset.tar`) and prints its CID. `catalog_ids.tsv` is
deliberately NOT in the tarball: a cardset carries no oracle ids, titles, or
other worldly identity.

`generate_title_catalog.rs` emits the dense presentation title table
(`presentation/title_catalog.tsv`) from the Scryfall snapshot joined by
Oracle id plus the frozen token-genesis source ordered by
`lib/token_genesis.rs` — the face-aware emitter preserved at
`archive/ip-clean-title-catalog-emitter`, now ported onto main's unified
`lib/scryfall_bulk.rs`. Its `--verify` mode re-checks an existing table.
Regenerating the committed table requires pointing `--catalog` at the
identity-assigning DeepScry catalog (this repository's own `catalog_ids.tsv`,
the default, carries no `metadata:` header field and is rejected):

```sh
rust-script scripts/generate_title_catalog.rs \
  --catalog <DeepScry>/src/engine/assets/card_catalog.tsv \
  --token-source <historical-named-token-source>/tokenscripts
```

`generate_body_catalog.rs` emits the sparse SS4 presentation body table
(`presentation/body_catalog.tsv`) from the same unified DeepScry catalog.
Card bodies come from Scryfall Oracle text joined by Oracle id. Token bodies
come from the historical named token scripts in the frozen order supplied by
`lib/token_genesis.rs`; blank Oracle fields are omitted, as SS4 permits. The
producer preserves a multi-face definition's face order in one cell, with one
blank line between face bodies, then applies SS4's `\\n`, `\\t`, and `\\\\`
escapes. It fails on missing Oracle identities, conflicting highest-precedence
Scryfall bodies, a changed token genesis block, and stale catalog stamps.

```sh
rust-script scripts/generate_body_catalog.rs \
  --catalog <DeepScry>/src/engine/assets/card_catalog.tsv \
  --token-source <historical-named-token-source>/tokenscripts

rust-script scripts/generate_body_catalog.rs \
  --catalog <DeepScry>/src/engine/assets/card_catalog.tsv \
  --verify presentation/body_catalog.tsv
```

`make_artpack.rs` emits the `kind=uuid-scheme` artpack table
(`presentation/artpack_scryfall_uuid.tsv`): one Scryfall printing UUID per
catalog id; clients compute image URLs with the `layout=scryfall-cdn-v1`
function declared in its header. Printing selection prefers non-art-series,
real-image, non-digital, English printings, then the oldest release, then
the smallest UUID — fully deterministic.

`make_provenance.rs` emits the dense id-to-oracle_id provenance table
(`presentation/provenance_oracle_ids.tsv`) from `catalog_ids.tsv`. It is a
skin-side artifact (worldly identity), never part of a cardset.

`make_skin_manifest.rs` assembles the skin manifest — canonical JSON binding
the cardset, titles, and optional bodies/artpack/provenance references, each
`{cid, size, hints[]}` with hint URLs inside the hash — and prints the
manifest's own CID, which names the whole skin.

`extract_card_skin.rs` predates the ratified package; its combined
titles+Oracle-text JSON remains a local-only cache artifact. Durable skin
artifacts are the TSV/manifest family above.
