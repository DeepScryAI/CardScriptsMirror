# Presentation title catalog

`title_catalog.tsv` is the complete, dense catalog-ID-to-title source for
title-only presentation skins: one `id<TAB>title` row for every card and token
in DeepScry's unified identity catalog. It is layout-independent presentation data — a skin, not
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

Card titles come from a Scryfall bulk snapshot joined by Oracle ID. Token
titles come from the historical named token source, ordered by the exact same
frozen genesis helper that generated the anonymous `tokens/` trie; there is no
second token identity ledger. Reversible
cards carry their Oracle identity on card faces with a null top-level
`oracle_id`, so a joiner must fall back to faces or it silently drops all of
them. The emitter that produced this table is
`scripts/generate_title_catalog.rs` on this branch — the face-aware emitter
originally preserved at tag `archive/ip-clean-title-catalog-emitter`, since
ported onto main's unified `scripts/lib/scryfall_bulk.rs` (DeepScry issue
ds-5432). Port verification (2026-08-23): the ported emitter regenerates
this table BYTE-IDENTICALLY from the 2026-08-22 Scryfall snapshot.

Unified-token verification (2026-08-23): rows 1 through 35,307 remain the
Scryfall-backed card titles, and rows 35,308 through 36,144 are the 837 frozen
token definitions. Two token rows carry ordered double-face titles. The
header's `catalog_identity` matches the title-free unified DeepScry catalog.

## The other skin artifacts (ratified SS0-SS5 package)

This directory also carries the Wizards skin's other generated tables (see
`scripts/README.md` and, normatively, `docs/CARD_SKIN_FORMATS.md` in the
DeepScry repository):

- `artpack_scryfall_uuid.tsv` — the `kind=uuid-scheme` artpack: catalog id to
  Scryfall printing UUID; clients compute image URLs with the
  `layout=scryfall-cdn-v1` function in its header. Emitted by
  `scripts/make_artpack.rs`.
- `provenance_oracle_ids.tsv` — dense catalog id to Scryfall `oracle_id`.
  Worldly identity, deliberately a skin-side artifact (cardsets are
  anonymous). Emitted by `scripts/make_provenance.rs`.
- `skins/` — canonical-JSON skin manifests binding a cardset CID with titles
  and the optional tables above. A manifest's own CID names the whole skin;
  the minted CIDs are recorded in the commit messages that add or update
  these files and in DeepScry issue ds-5432.

Every artifact is content-addressed with the pinned profile (CIDv1,
sha2-256, raw codec, single block, base32); the CID is over the exact
committed bytes, so editing any of these files by hand mints a different
object — regenerate with the producer scripts instead.
