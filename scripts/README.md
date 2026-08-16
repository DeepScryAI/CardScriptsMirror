# Pipeline scripts

All executable tooling on this branch is safe Rust run through `rust-script`.
Each script embeds pinned Cargo dependency requirements and keeps downloaded or
diagnostic data below the gitignored `.cache/` directory.

`extract_catalog_ids.rs` removes plaintext presentation names from DeepScry's
append-only numeric catalog while retaining the numeric ID, Scryfall Oracle
UUID, and a one-way name digest needed to validate aliases.

`generate_uuid_trie.rs` owns Oracle-identity mapping, structured Forge-script
sanitization, and numeric-trie generation.

`scan_scryfall_ip.rs` compiles all normalized Scryfall titles and Oracle texts
into an overlapping Aho-Corasick automaton and scans tracked repository text.
It shares the downloader/parser in `lib/scryfall_bulk.rs` with the generator.
By default it traverses submodules. Use `--exclude-submodules` only when the
audit intentionally treats independently versioned submodule repositories as
opaque gitlinks; the selected scope is recorded in the JSON report.
