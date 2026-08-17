# Pipeline scripts

All executable tooling on this branch is safe Rust run through `rust-script`.
Each script embeds pinned Cargo dependency requirements and keeps downloaded or
diagnostic data below the gitignored `.cache/` directory.

`extract_catalog_ids.rs` removes plaintext presentation names from DeepScry's
append-only numeric catalog while retaining the numeric ID, Scryfall Oracle
UUID, a one-way name digest needed to validate aliases, and a frozen
YEAR+LETTER origin-set ID backed by `set_ids.tsv`.

`rewrite_origin_sets.rs` applies that frozen table to the existing numeric
trie. It validates the numeric path identity and rewrites exactly one
`OriginSet:` field, leaving executable DSL records byte-for-byte unchanged.

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
