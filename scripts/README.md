# Pipeline scripts

All executable tooling on this branch is safe Rust run through `rust-script`.
Each script embeds pinned Cargo dependency requirements and keeps downloaded or
diagnostic data below the gitignored `.cache/` directory.

`extract_catalog_ids.rs` removes plaintext presentation names from DeepScry's
append-only numeric catalog while retaining the numeric ID, Scryfall Oracle
UUID, and a one-way name digest needed to validate aliases.

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

`generate_title_catalog.rs` emits the dense presentation title catalog that
DeepScry's strict title-skin loader consumes. It reads only `#id` and
`oracle_id` from the numeric bridge and takes every title from the cached
Scryfall snapshot, so it keeps working after DeepScry's catalog loses its
`name` column. The emitted header carries `catalog_identity`: the SHA-256 of
the exact catalog file that assigned those numeric IDs. Its `--verify` mode
re-checks an existing table against a catalog and rejects an unstamped,
mis-stamped, sparse, or tampered one.
