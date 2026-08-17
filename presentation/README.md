# Presentation title catalog

`title_catalog.tsv` is the complete, dense `CardCatalogId`-to-title source for
temporary title-only skins. It is owned by CardScriptsMirror so DeepScry can
remove its checked-in title-bearing catalog without making title rendering fall
back to main-repository data.

The table uses the strict TSV contract consumed by DeepScry's
`scripts/extract_catalog_title_skin.py` and `bin/namecards`: a metadata header
records the source snapshot, exact row count, and SHA-256 of the body, followed
by dense `id<TAB>title` rows. Consumers must reject a missing, malformed, or
partial table; they must not fall back to DeepScry's former
`src/engine/assets/card_catalog.tsv` path.

It is title-only: it deliberately contains no Oracle text, face text, artwork,
or image URLs. The data is presentation material, not a card-definition loader.
