# Presentation title catalog

`title_catalog.tsv` is the complete, dense catalog-ID-to-title source for
title-only presentation skins: one `id<TAB>title` row for every row of
`catalog_ids.tsv`. It is layout-independent presentation data — a skin, not
card-definition data — and it is deliberately title-only: no Oracle text, no
face text, no artwork, and no image URLs.

## Contract

The first line is a self-verifying metadata header consumed by DeepScry's
browser loader (`web/ts/card_skin.ts`) and `bin/namecards`:

```text
#id<TAB>title<TAB>metadata: v=1 ... catalog_identity=<sha256> cards=<count> body_sha256=<sha256>
```

- `catalog_identity` is the SHA-256 of the complete catalog file the titles
  were joined against — DeepScry's embedded
  `src/engine/assets/card_catalog.tsv`. Consumers reject the table when this
  stamp does not match the running catalog, so the table must be re-emitted
  in the same change that alters that catalog, or skin loading breaks at
  that commit.
- `body_sha256` is the SHA-256 of everything after the header line. Never
  hand-edit rows without restamping.
- Rows are dense (ids `1..cards`, exactly one title each). Consumers must
  reject a missing, malformed, or partial table; a partial skin would make a
  missing title look like a game regression instead of a load error.

## Provenance and verification

Titles come from a Scryfall bulk snapshot joined by Oracle ID. Reversible
cards carry their Oracle identity on card faces with a null top-level
`oracle_id`, so a joiner must fall back to faces or it silently drops all of
them. The emitter that produced this table,
`scripts/generate_title_catalog.rs`, together with the face-aware
`scripts/lib/scryfall_bulk.rs` it requires, is preserved at tag
`archive/ip-clean-title-catalog-emitter`; porting it onto main's unified
pipeline is tracked in DeepScry issue ds-5432.

Fold-time verification (2026-08-22): for all 35,307 ids, the SHA-256 of the
title in this table equals the `name_sha256` recorded for the same id in
this repository's `catalog_ids.tsv`, and the header's `catalog_identity`
matches DeepScry's embedded catalog on its integration and main branches.
