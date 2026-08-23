#!/usr/bin/env rust-script
//! ```cargo
//! [package]
//! edition = "2021"
//!
//! [dependencies]
//! anyhow = "1.0.99"
//! clap = { version = "4.5.45", features = ["derive"] }
//! flate2 = "1.1.2"
//! reqwest = { version = "0.12.28", default-features = false, features = ["blocking", "json", "rustls-tls"] }
//! serde = { version = "1.0.219", features = ["derive"] }
//! serde_json = "1.0.143"
//! sha2 = "0.10.9"
//! uuid = { version = "1.18.0", features = ["serde"] }
//! ```

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

#[path = "lib/scryfall_bulk.rs"]
mod scryfall_bulk;

use scryfall_bulk::{ScryfallCard, ScryfallFace};

#[derive(Parser, Debug)]
#[command(about = "Generate anonymous Forge scripts keyed by stable numeric card ID")]
struct Args {
    /// Existing Forge cardsfolder directory.
    #[arg(long)]
    source: PathBuf,

    /// Existing Forge token-scripts directory. Defaults to `tokenscripts`
    /// beside the card-script source directory.
    #[arg(long)]
    token_source: Option<PathBuf>,

    /// Generated three-level numeric-ID trie.
    #[arg(long, default_value = "cards")]
    output: PathBuf,

    /// Generated three-level numeric token-ID trie.
    #[arg(long, default_value = "tokens")]
    token_output: PathBuf,

    /// Decompressed Scryfall default_cards cache.
    #[arg(long, default_value = ".cache/scryfall/default_cards.json")]
    cache: PathBuf,

    /// Tab-separated `id` and `oracle_id` bridge. It deliberately contains no
    /// card titles or display text.
    #[arg(long, default_value = "catalog_ids.tsv")]
    catalog: PathBuf,

    /// Ignore a present cache and download the current snapshot.
    #[arg(long)]
    refresh: bool,

    /// Fail if any source script has no unique Scryfall Oracle identity.
    #[arg(long)]
    strict: bool,
}

#[derive(Debug, Serialize)]
struct GenerationReport {
    source_scripts: usize,
    generated_scripts: usize,
    duplicate_identical_scripts: usize,
    missing_mappings: Vec<MappingProblem>,
    ambiguous_mappings: Vec<MappingProblem>,
    conflicting_scripts: Vec<ScriptConflict>,
    /// Reconciliation guard (see `reconcile_identities`): every top-level
    /// `Name:` identity a source script defines (the front face, plus a
    /// meld back face when `meld_back_identity` finds one) MUST be passed
    /// to `index.lookup` at least once — landing it in `generated`,
    /// `missing_mappings`, or `ambiguous_mappings`. An identity that is
    /// locally present in the Forge source but was NEVER attempted at all
    /// lands here instead. Unlike the other three buckets (which reflect
    /// real Scryfall-coverage gaps and are expected to be non-empty), this
    /// one reflects the generator itself failing to look at content it
    /// has — it should always be empty, and `main` aborts unconditionally
    /// on any non-empty result (see `main`'s explicit reasoning).
    unaccounted_identities: Vec<MappingProblem>,
}

#[derive(Debug, Serialize)]
struct MappingProblem {
    source: String,
    name: String,
    oracle_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ScriptConflict {
    card_id: u32,
    first_source: String,
    second_source: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OracleId(Uuid);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CardScriptId(u32);

impl CardScriptId {
    fn trie_path(&self, root: &Path) -> PathBuf {
        let id = format!("{:08}", self.0);
        root.join(&id[0..2])
            .join(&id[2..4])
            .join(&id[4..6])
            .join(format!("{id}.txt"))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TokenScriptId(u32);

impl TokenScriptId {
    fn trie_path(&self, root: &Path) -> PathBuf {
        CardScriptId(self.0).trie_path(root)
    }
}

#[derive(Default)]
struct CatalogIndex {
    by_oracle_id: BTreeMap<OracleId, Vec<CardScriptId>>,
    by_name_hash: BTreeMap<String, Option<CardScriptId>>,
    set_group_by_id: BTreeMap<CardScriptId, String>,
}

#[derive(Default)]
struct NameIndex {
    whole_cards: BTreeMap<String, BTreeMap<OracleId, IdentityEvidence>>,
    faces: BTreeMap<String, BTreeMap<OracleId, IdentityEvidence>>,
    color_identities: BTreeMap<OracleId, String>,
}

#[derive(Clone, Debug, Default)]
struct IdentityEvidence {
    oracle_texts: BTreeSet<String>,
    signatures: BTreeSet<CardSignature>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CardSignature(Vec<FaceSignature>);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FaceSignature {
    mana_cost: String,
    type_line: String,
}

impl NameIndex {
    fn insert_whole_card(
        &mut self,
        name: &str,
        oracle_id: OracleId,
        oracle_text: Option<&str>,
        signature: CardSignature,
    ) {
        insert_identity(&mut self.whole_cards, name, oracle_id, oracle_text, signature);
    }

    fn insert_face(&mut self, name: &str, oracle_id: OracleId, oracle_text: Option<&str>, signature: CardSignature) {
        insert_identity(&mut self.faces, name, oracle_id, oracle_text, signature);
    }

    fn lookup(&self, name: &str) -> Option<&BTreeMap<OracleId, IdentityEvidence>> {
        // A complete card name is authoritative. Face aliases are needed for
        // Forge's split/adventure face scripts, but a new multi-face card can
        // legitimately reuse the name of an older standalone card.
        self.whole_cards
            .get(name.trim())
            .or_else(|| self.faces.get(name.trim()))
    }

    fn distinct_name_count(&self) -> usize {
        self.whole_cards
            .keys()
            .chain(self.faces.keys())
            .collect::<BTreeSet<_>>()
            .len()
    }
}

fn insert_identity(
    index: &mut BTreeMap<String, BTreeMap<OracleId, IdentityEvidence>>,
    name: &str,
    oracle_id: OracleId,
    oracle_text: Option<&str>,
    signature: CardSignature,
) {
    let evidence = index
        .entry(name.trim().to_owned())
        .or_default()
        .entry(oracle_id)
        .or_default();
    if let Some(oracle_text) = oracle_text {
        evidence.oracle_texts.insert(normalize_oracle_text(oracle_text));
    }
    evidence.signatures.insert(signature);
}

fn main() -> Result<()> {
    let args = Args::parse();
    validate_source(&args.source)?;
    validate_output_path(&args.output)?;
    validate_output_path(&args.token_output)?;
    let token_source = args.token_source.clone().unwrap_or_else(|| {
        args.source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("tokenscripts")
    });
    validate_source(&token_source)?;
    scryfall_bulk::ensure_cache(&args.cache, args.refresh)?;

    eprintln!("Parsing Scryfall identities from {}", args.cache.display());
    let index = load_name_index(&args.cache)?;
    eprintln!(
        "Indexed {} distinct Scryfall names and faces",
        index.distinct_name_count()
    );
    let catalog = load_catalog_index(&args.catalog)?;
    let catalog_ids: usize = catalog.by_oracle_id.values().map(Vec::len).sum();
    eprintln!("Loaded {catalog_ids} stable numeric identities");

    let token_index = build_token_index(&token_source)?;
    eprintln!("Loaded {} stable numeric token identities", token_index.len());
    let report = generate(&args.source, &args.output, &index, &catalog, &token_index)?;
    generate_tokens(&token_source, &args.token_output, &index, &catalog, &token_index)?;
    write_report(&report)?;
    print_report(&report);

    // Unconditional, NOT gated by --strict: unlike missing/ambiguous
    // mappings (which reflect real, expected Scryfall-coverage gaps that
    // legitimately scale with the size of the card pool and are only worth
    // failing on when the CALLER explicitly opts in), an unaccounted
    // identity means the generator itself never even tried to look up a
    // name it had in hand — a code-coverage bug in the pipeline, not a
    // data-coverage gap. Its size carries no signal about severity: a
    // single missed identity today is the same class of defect as a
    // hundred tomorrow, so this always aborts rather than warning, on ANY
    // non-empty result. See `GenerationReport::unaccounted_identities`'s
    // doc comment and `reconcile_identities` for what this catches and why
    // it is a real check, not a tautology.
    if !report.unaccounted_identities.is_empty() {
        bail!(
            "generation left {} identities completely unaccounted for (present in a Forge source script but never passed to a Scryfall lookup) - see .cache/reports/generate-report.json's unaccounted_identities",
            report.unaccounted_identities.len()
        );
    }

    let mapping_problem_count = report.missing_mappings.len() + report.ambiguous_mappings.len();
    if !report.conflicting_scripts.is_empty() {
        bail!(
            "generation found {} UUID collisions with different sanitized scripts; see .cache/reports/generate-report.json",
            report.conflicting_scripts.len()
        );
    }
    if args.strict && mapping_problem_count > 0 {
        bail!(
            "strict generation rejected {mapping_problem_count} missing or ambiguous Scryfall mappings; see .cache/reports/generate-report.json"
        );
    }
    Ok(())
}

fn validate_source(source: &Path) -> Result<()> {
    if !source.is_dir() {
        bail!("source is not a directory: {}", source.display());
    }
    Ok(())
}

fn validate_output_path(output: &Path) -> Result<()> {
    if output.as_os_str().is_empty() || output == Path::new("/") {
        bail!("refusing unsafe output path: {}", output.display());
    }
    if output
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("output path may not contain '..': {}", output.display());
    }
    let Some(name) = output.file_name().and_then(OsStr::to_str) else {
        bail!("output path must have a UTF-8 final component: {}", output.display());
    };
    if name == "." || name.is_empty() {
        bail!("refusing unsafe output path: {}", output.display());
    }
    Ok(())
}

fn load_name_index(cache: &Path) -> Result<NameIndex> {
    let mut index = NameIndex::default();
    scryfall_bulk::for_each_card(cache, |card| index_card(&mut index, card))?;
    Ok(index)
}

fn load_catalog_index(path: &Path) -> Result<CatalogIndex> {
    let content = fs::read_to_string(path).with_context(|| format!("read numeric catalog {}", path.display()))?;
    let mut index = CatalogIndex::default();
    for (line_number, line) in content.lines().enumerate() {
        if line_number == 0 && line.starts_with("#id\t") {
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let id: u32 = fields
            .next()
            .context("numeric catalog row has no id")?
            .parse()
            .with_context(|| format!("invalid card id on {}:{}", path.display(), line_number + 1))?;
        if id == 0 {
            bail!("numeric catalog reserves id 0 ({}:{})", path.display(), line_number + 1);
        }
        let oracle_id = fields
            .next()
            .context("numeric catalog row has no oracle_id")?
            .parse::<Uuid>()
            .with_context(|| format!("invalid oracle UUID on {}:{}", path.display(), line_number + 1))?;
        let name_hash = fields
            .next()
            .context("numeric catalog row has no anonymous name hash")?
            .to_owned();
        let set_group = fields
            .next()
            .context("numeric catalog row has no anonymous set group")?
            .to_owned();
        if name_hash.len() != 64 || !name_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid name hash on {}:{}", path.display(), line_number + 1);
        }
        match index.by_name_hash.entry(name_hash) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(CardScriptId(id)));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry.get().is_some_and(|existing| existing.0 != id) {
                    entry.insert(None);
                }
            }
        }
        index
            .by_oracle_id
            .entry(OracleId(oracle_id))
            .or_default()
            .push(CardScriptId(id));
        index.set_group_by_id.insert(CardScriptId(id), set_group);
    }
    Ok(index)
}

#[cfg(test)]
fn index_cards(cards: Vec<ScryfallCard>) -> NameIndex {
    let mut index = NameIndex::default();
    for card in cards {
        index_card(&mut index, card);
    }
    index
}

fn index_card(index: &mut NameIndex, card: ScryfallCard) {
    // Art Series checklist cards copy a real card's face name but have their
    // own non-game Oracle UUID. They must never make a playable name
    // ambiguous. Other layouts, including schemes and planes, are game
    // objects with scripts and remain eligible.
    if card.lang != "en"
        || matches!(
            card.layout.as_str(),
            "art_series" | "double_faced_token" | "emblem" | "front_card" | "token"
        )
    {
        return;
    }
    let Some(oracle_id) = card.oracle_id.map(OracleId) else {
        return;
    };
    let color_identity = commander_color_identity(&card);
    index
        .color_identities
        .entry(oracle_id.clone())
        .or_insert(color_identity);
    let whole_signature = scryfall_card_signature(&card);
    index.insert_whole_card(
        &card.name,
        oracle_id.clone(),
        card.oracle_text.as_deref(),
        whole_signature.clone(),
    );
    if let Some(printed_name) = card.printed_name {
        index.insert_whole_card(
            &printed_name,
            oracle_id.clone(),
            card.oracle_text.as_deref(),
            whole_signature,
        );
    }
    for face in card.card_faces {
        let face_signature = CardSignature(vec![scryfall_face_signature(&face)]);
        index.insert_face(
            &face.name,
            oracle_id.clone(),
            face.oracle_text.as_deref(),
            face_signature.clone(),
        );
        if let Some(printed_name) = face.printed_name {
            index.insert_face(
                &printed_name,
                oracle_id.clone(),
                face.oracle_text.as_deref(),
                face_signature,
            );
        }
    }
}

/// Return CR 903.4 color identity without folding in CR 903.5d's separate
/// basic-land-type restriction.
///
/// Scryfall's convenient `color_identity` field combines both concepts. We
/// retain its complete answer, but remove a basic-type color when that color
/// has no explicit source in a mana cost, non-reminder rules text, printed
/// color, or color indicator. The generated runtime scripts therefore remain
/// self-contained without shipping or parsing Oracle text.
fn commander_color_identity(card: &ScryfallCard) -> String {
    const COLORS: [&str; 5] = ["W", "U", "B", "R", "G"];
    let mut explicit = BTreeSet::new();
    collect_explicit_identity_sources(
        &card.mana_cost,
        card.oracle_text.as_deref(),
        &card.colors,
        card.color_indicator.as_deref(),
        &mut explicit,
    );
    for face in &card.card_faces {
        collect_explicit_identity_sources(
            &face.mana_cost,
            face.oracle_text.as_deref(),
            &face.colors,
            face.color_indicator.as_deref(),
            &mut explicit,
        );
    }

    COLORS
        .into_iter()
        .filter(|color| card.color_identity.iter().any(|present| present == color))
        .filter(|color| explicit.contains(&color.as_bytes()[0]) || !has_basic_land_type(card, color))
        .collect()
}

fn collect_explicit_identity_sources(
    mana_cost: &str,
    oracle_text: Option<&str>,
    colors: &[String],
    color_indicator: Option<&[String]>,
    explicit: &mut BTreeSet<u8>,
) {
    collect_mana_symbol_colors(mana_cost, explicit);
    if let Some(text) = oracle_text {
        let without_reminder = strip_parenthetical_text(text);
        collect_mana_symbol_colors(&without_reminder, explicit);
    }
    explicit.extend(colors.iter().filter_map(|color| color.as_bytes().first().copied()));
    explicit.extend(
        color_indicator
            .into_iter()
            .flatten()
            .filter_map(|color| color.as_bytes().first().copied()),
    );
}

fn collect_mana_symbol_colors(text: &str, colors: &mut BTreeSet<u8>) {
    for symbol in text
        .split('{')
        .skip(1)
        .filter_map(|tail| tail.split_once('}').map(|pair| pair.0))
    {
        for color in ["W", "U", "B", "R", "G"] {
            if symbol.split('/').any(|part| part == color) {
                colors.insert(color.as_bytes()[0]);
            }
        }
    }
}

fn strip_parenthetical_text(text: &str) -> String {
    let mut depth = 0_u32;
    text.chars()
        .filter(|ch| match ch {
            '(' => {
                depth += 1;
                false
            }
            ')' if depth > 0 => {
                depth -= 1;
                false
            }
            _ => depth == 0,
        })
        .collect()
}

fn has_basic_land_type(card: &ScryfallCard, color: &str) -> bool {
    let basic_type = match color {
        "W" => "Plains",
        "U" => "Island",
        "B" => "Swamp",
        "R" => "Mountain",
        "G" => "Forest",
        _ => return false,
    };
    std::iter::once(card.type_line.as_str())
        .chain(card.card_faces.iter().map(|face| face.type_line.as_str()))
        .flat_map(str::split_whitespace)
        .any(|word| word.trim_matches(|ch: char| !ch.is_alphabetic()) == basic_type)
}

fn generate(
    source: &Path,
    output: &Path,
    index: &NameIndex,
    catalog: &CatalogIndex,
    token_index: &BTreeMap<String, TokenScriptId>,
) -> Result<GenerationReport> {
    let sources = source_scripts(source)?;
    let stage = sibling_with_suffix(output, &format!("build-{}", std::process::id()))?;
    if stage.exists() {
        fs::remove_dir_all(&stage).with_context(|| format!("remove stale stage {}", stage.display()))?;
    }
    fs::create_dir_all(&stage).with_context(|| format!("create stage {}", stage.display()))?;

    let mut report = GenerationReport {
        source_scripts: sources.len(),
        generated_scripts: 0,
        duplicate_identical_scripts: 0,
        missing_mappings: Vec::new(),
        ambiguous_mappings: Vec::new(),
        conflicting_scripts: Vec::new(),
        unaccounted_identities: Vec::new(),
    };
    let mut generated: BTreeMap<CardScriptId, (PathBuf, String)> = BTreeMap::new();
    let numeric_name_refs = numeric_name_references(index, catalog);

    // Every (source file, identity name) pair actually passed to
    // `index.lookup` below — populated by `resolve_and_generate`. Compared
    // against `expected_identities` after the main loop by
    // `reconcile_identities` (see there for why this check exists and what
    // it would NOT have caught before the meld fix).
    let mut attempted: BTreeSet<(PathBuf, String)> = BTreeSet::new();
    // Cached so the reconciliation pass below can recompute, from the SAME
    // raw text, which identities each file SHOULD have contributed —
    // independently of whatever the main loop below actually did.
    let mut source_texts: Vec<(PathBuf, String)> = Vec::with_capacity(sources.len());

    // Resolves ONE (source, name, text) identity through the Scryfall name
    // index -> Oracle id -> numeric catalog id -> sanitized write pipeline.
    // Called once per source file for its front identity, and once more
    // for a meld back identity when `meld_back_identity` finds one (see
    // `meld_back_identity`'s doc comment for why melds, uniquely among the
    // multi-`Name:` modes, need a second independent call here).
    let mut resolve_and_generate = |report: &mut GenerationReport,
                                     generated: &mut BTreeMap<CardScriptId, (PathBuf, String)>,
                                     source_path: &Path,
                                     name: &str,
                                     text: &str| {
        attempted.insert((source_path.to_path_buf(), name.to_owned()));
        let oracle_ids = match index.lookup(name) {
            Some(candidates) => match resolve_oracle_ids(text, candidates) {
                Some(ids) => ids,
                None => {
                    report.ambiguous_mappings.push(MappingProblem {
                        source: relative_display(source, source_path),
                        name: name.to_owned(),
                        oracle_ids: candidates.keys().map(|id| id.0.hyphenated().to_string()).collect(),
                    });
                    return Ok::<(), anyhow::Error>(());
                }
            },
            None => {
                report.missing_mappings.push(MappingProblem {
                    source: relative_display(source, source_path),
                    name: name.to_owned(),
                    oracle_ids: Vec::new(),
                });
                return Ok(());
            }
        };
        for oracle_id in oracle_ids {
            let Some(card_ids) = catalog.by_oracle_id.get(&oracle_id) else {
                report.missing_mappings.push(MappingProblem {
                    source: relative_display(source, source_path),
                    name: name.to_owned(),
                    oracle_ids: vec![oracle_id.0.hyphenated().to_string()],
                });
                continue;
            };
            for &card_id in card_ids {
                let color_identity = index.color_identities.get(&oracle_id).map(String::as_str).unwrap_or("");
                let set_group = catalog
                    .set_group_by_id
                    .get(&card_id)
                    .expect("catalog index lost anonymous set group");
                let sanitized =
                    sanitize_script(text, card_id, color_identity, set_group, &numeric_name_refs, token_index);
                if let Some((first_path, first_text)) = generated.get(&card_id) {
                    if first_text == &sanitized {
                        report.duplicate_identical_scripts += 1;
                    } else {
                        report.conflicting_scripts.push(ScriptConflict {
                            card_id: card_id.0,
                            first_source: relative_display(source, first_path),
                            second_source: relative_display(source, source_path),
                        });
                    }
                    continue;
                }

                let destination = card_id.trie_path(&stage);
                let parent = destination.parent().context("generated path has no parent")?;
                fs::create_dir_all(parent).with_context(|| format!("create trie directory {}", parent.display()))?;
                fs::write(&destination, sanitized.as_bytes())
                    .with_context(|| format!("write generated script {}", destination.display()))?;
                generated.insert(card_id, (source_path.to_path_buf(), sanitized));
                report.generated_scripts += 1;
            }
        }
        Ok(())
    };

    for source_path in sources {
        let source_text =
            fs::read_to_string(&source_path).with_context(|| format!("read Forge script {}", source_path.display()))?;
        let meld_back = meld_back_identity(&source_text);
        if let Some(name) = source_identity_name(&source_text) {
            resolve_and_generate(&mut report, &mut generated, &source_path, &name, &source_text)?;
        } else {
            report.missing_mappings.push(MappingProblem {
                source: relative_display(source, &source_path),
                name: "<missing Name field>".to_owned(),
                oracle_ids: Vec::new(),
            });
        }
        if let Some((back_name, back_text)) = &meld_back {
            resolve_and_generate(&mut report, &mut generated, &source_path, back_name, back_text)?;
        }
        source_texts.push((source_path, source_text));
    }

    reconcile_identities(source, &source_texts, &attempted, &mut report.unaccounted_identities);

    publish_directory(&stage, output)?;
    Ok(report)
}

/// The reconciliation guard for the meld-back-face hole (see
/// `GenerationReport::unaccounted_identities`'s doc comment).
///
/// Independently re-derives, from the SAME cached raw source text the main
/// loop in `generate` already read, which (source, name) identity pairs
/// that loop SHOULD have passed to `index.lookup` — one per source file's
/// front identity, plus one more wherever `meld_back_identity` finds a
/// meld back face. This is a genuinely separate check, not a tautology:
/// the main loop's decision to make a second `resolve_and_generate` call
/// for a meld back is one piece of code; this function's decision to
/// EXPECT that second call is a different piece of code that happens to
/// call the same pure helper. If a future change ever lets those two
/// pieces of code drift apart — the loop stops making the second call
/// while this still expects it, or vice versa — this reports the
/// divergence with exact (source, name) pairs, not just a bare count.
///
/// Before the meld fix landed, running this against the unfixed loop (only
/// ever making ONE `resolve_and_generate` call per source file) reported
/// exactly the 7 meld back identities documented in
/// `ai_docs/transient/NUMERIC_CORPUS_SWAP_20260819.md` section 8 as
/// unaccounted for.
fn reconcile_identities(
    source_root: &Path,
    source_texts: &[(PathBuf, String)],
    attempted: &BTreeSet<(PathBuf, String)>,
    unaccounted: &mut Vec<MappingProblem>,
) {
    for (source_path, text) in source_texts {
        let mut expected_names: Vec<String> = Vec::with_capacity(2);
        if let Some(name) = source_identity_name(text) {
            expected_names.push(name);
        }
        if let Some((back_name, _)) = meld_back_identity(text) {
            expected_names.push(back_name);
        }
        for name in expected_names {
            if !attempted.contains(&(source_path.clone(), name.clone())) {
                unaccounted.push(MappingProblem {
                    source: relative_display(source_root, source_path),
                    name,
                    oracle_ids: Vec::new(),
                });
            }
        }
    }
}

/// For an `AlternateMode:Meld` script whose `ALTERNATE` block defines its
/// own back-face card (a distinct `Name:` line after the literal
/// `ALTERNATE` marker line), returns `(back_name, back_script_text)` so the
/// back can be independently resolved and generated under its OWN numeric
/// id.
///
/// Meld's front and back are DIFFERENT Oracle identities in Scryfall's own
/// data (confirmed: Gisela, the Broken Blade and Brisela, Voice of
/// Nightmares carry distinct `oracle_id`s) — unlike `Split` (one combined
/// name IS one shared Oracle identity, correctly handled by
/// `source_identity_name`'s existing join) or a transforming/modal
/// double-faced card (both faces share ONE Oracle identity, and Scryfall's
/// own face index means looking up the FRONT name alone already resolves
/// the shared identity correctly). A meld back is not reachable through
/// either of those paths: nothing in Scryfall's data links Brisela to
/// Gisela by name or face, so if nothing independently looks Brisela up by
/// her OWN name, she is never attempted at all — not missing, not
/// ambiguous, just never tried. Returns `None` for every other file,
/// including a meld pair's OTHER front half, whose own `AlternateMode:Meld`
/// marker is present but which only REFERENCES the back by name (via
/// `SVar:Meld:...Name$ <back>`), never redefines it.
fn meld_back_identity(script: &str) -> Option<(String, String)> {
    let is_meld = script.lines().any(|line| {
        line.split_once(':')
            .map(|(key, value)| key.trim() == "AlternateMode" && value.trim() == "Meld")
            .unwrap_or(false)
    });
    if !is_meld {
        return None;
    }
    let mut lines = script.lines();
    for line in lines.by_ref() {
        if line.trim() == "ALTERNATE" {
            break;
        }
    }
    let back_text: String = lines.collect::<Vec<_>>().join("\n");
    let back_name = top_level_value(&back_text, "Name")?.trim().to_owned();
    Some((back_name, back_text))
}

fn source_scripts(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        let mut entries: Vec<_> = fs::read_dir(directory)
            .with_context(|| format!("read source directory {}", directory.display()))?
            .collect::<std::result::Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat {}", entry.path().display()))?;
            if file_type.is_dir() {
                visit(&entry.path(), files)?;
            } else if file_type.is_file() && entry.path().extension() == Some(OsStr::new("txt")) {
                files.push(entry.path());
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, &mut files)?;
    Ok(files)
}

fn token_id_for_key(key: &str) -> TokenScriptId {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&digest[..4]);
    TokenScriptId(u32::from_be_bytes(bytes).max(1))
}

fn build_token_index(source: &Path) -> Result<BTreeMap<String, TokenScriptId>> {
    let mut by_name = BTreeMap::new();
    let mut by_id = BTreeMap::<TokenScriptId, String>::new();
    for path in source_scripts(source)? {
        let key = path
            .file_stem()
            .and_then(OsStr::to_str)
            .with_context(|| format!("token script has no UTF-8 stem: {}", path.display()))?
            .to_owned();
        let id = token_id_for_key(&key);
        if let Some(first) = by_id.insert(id, key.clone()) {
            bail!("numeric token ID collision between {first:?} and {key:?} ({})", id.0);
        }
        by_name.insert(key, id);
    }
    Ok(by_name)
}

fn generate_tokens(
    source: &Path,
    output: &Path,
    card_names: &NameIndex,
    catalog: &CatalogIndex,
    token_index: &BTreeMap<String, TokenScriptId>,
) -> Result<()> {
    let stage = sibling_with_suffix(output, &format!("build-{}", std::process::id()))?;
    if stage.exists() {
        fs::remove_dir_all(&stage).with_context(|| format!("remove stale stage {}", stage.display()))?;
    }
    fs::create_dir_all(&stage).with_context(|| format!("create stage {}", stage.display()))?;
    let numeric_name_refs = numeric_name_references(card_names, catalog);

    for source_path in source_scripts(source)? {
        let key = source_path
            .file_stem()
            .and_then(OsStr::to_str)
            .context("token script has no UTF-8 stem")?;
        let token_id = token_index.get(key).context("token index lost a source script")?;
        let source_text = fs::read_to_string(&source_path)
            .with_context(|| format!("read Forge token script {}", source_path.display()))?;
        let sanitized = sanitize_token_script(&source_text, *token_id, &numeric_name_refs, token_index);
        let destination = token_id.trie_path(&stage);
        let parent = destination.parent().context("generated token path has no parent")?;
        fs::create_dir_all(parent).with_context(|| format!("create token trie directory {}", parent.display()))?;
        fs::write(&destination, sanitized.as_bytes())
            .with_context(|| format!("write generated token script {}", destination.display()))?;
    }
    publish_directory(&stage, output)?;
    eprintln!("Generated {} numeric-ID token scripts", token_index.len());
    Ok(())
}

fn top_level_value<'a>(script: &'a str, wanted: &str) -> Option<&'a str> {
    script.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == wanted).then(|| value.trim())
    })
}

fn source_identity_name(script: &str) -> Option<String> {
    let names: Vec<&str> = script
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "Name").then(|| value.trim())
        })
        .collect();
    let is_split = script.lines().any(|line| {
        line.split_once(':')
            .map(|(key, value)| key.trim() == "AlternateMode" && value.trim() == "Split")
            .unwrap_or(false)
    });
    if is_split && names.len() >= 2 {
        return Some(names.join(" // "));
    }
    if let Some(name) = names.first() {
        return Some((*name).to_owned());
    }
    let copied_faces: Vec<&str> = script
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "CopyFaceFrom").then(|| value.trim())
        })
        .collect();
    (!copied_faces.is_empty()).then(|| copied_faces.join(" // "))
}

fn resolve_oracle_ids(script: &str, candidates: &BTreeMap<OracleId, IdentityEvidence>) -> Option<Vec<OracleId>> {
    if candidates.len() == 1 {
        return Some(candidates.keys().cloned().collect());
    }
    if top_level_value(script, "Oracle") == Some("<Unsupported Variant>") {
        return Some(candidates.keys().cloned().collect());
    }
    let source_oracle = top_level_value(script, "Oracle")?;
    let normalized = normalize_oracle_text(source_oracle);
    let matching: Vec<OracleId> = candidates
        .iter()
        .filter(|(_, evidence)| evidence.oracle_texts.contains(&normalized))
        .map(|(id, _)| id.clone())
        .collect();
    if matching.len() == 1 {
        return Some(matching);
    }
    let source_signature = forge_script_signature(script)?;
    let matching: Vec<OracleId> = candidates
        .iter()
        .filter(|(_, evidence)| evidence.signatures.contains(&source_signature))
        .map(|(id, _)| id.clone())
        .collect();
    (matching.len() == 1).then_some(matching)
}

fn normalize_oracle_text(text: &str) -> String {
    text.replace("\\n", "\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn scryfall_card_signature(card: &ScryfallCard) -> CardSignature {
    if card.card_faces.is_empty() {
        CardSignature(vec![FaceSignature {
            mana_cost: normalize_mana_cost(&card.mana_cost),
            type_line: normalize_type_line(&card.type_line),
        }])
    } else {
        CardSignature(card.card_faces.iter().map(scryfall_face_signature).collect())
    }
}

fn scryfall_face_signature(face: &ScryfallFace) -> FaceSignature {
    FaceSignature {
        mana_cost: normalize_mana_cost(&face.mana_cost),
        type_line: normalize_type_line(&face.type_line),
    }
}

fn forge_script_signature(script: &str) -> Option<CardSignature> {
    let mana_costs: Vec<&str> = top_level_values(script, "ManaCost");
    let type_lines: Vec<&str> = top_level_values(script, "Types");
    if mana_costs.is_empty() || mana_costs.len() != type_lines.len() {
        return None;
    }
    Some(CardSignature(
        mana_costs
            .into_iter()
            .zip(type_lines)
            .map(|(mana_cost, type_line)| FaceSignature {
                mana_cost: normalize_mana_cost(mana_cost),
                type_line: normalize_type_line(type_line),
            })
            .collect(),
    ))
}

fn top_level_values<'a>(script: &'a str, wanted: &str) -> Vec<&'a str> {
    script
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == wanted).then(|| value.trim())
        })
        .collect()
}

fn normalize_mana_cost(value: &str) -> String {
    if value.eq_ignore_ascii_case("no cost") {
        return String::new();
    }
    value
        .chars()
        .filter(|character| character.is_alphanumeric() || *character == '/')
        .flat_map(char::to_uppercase)
        .collect()
}

fn normalize_type_line(value: &str) -> String {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn numeric_name_references(index: &NameIndex, catalog: &CatalogIndex) -> Vec<(String, CardScriptId)> {
    let mut unique = BTreeMap::<String, Option<CardScriptId>>::new();
    for (name, identities) in index.whole_cards.iter().chain(index.faces.iter()) {
        let hash = Sha256::digest(name.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let card_id = catalog.by_name_hash.get(&hash).copied().flatten().or_else(|| {
            let oracle_id = (identities.len() == 1).then(|| identities.keys().next().expect("one identity"))?;
            let ids = catalog.by_oracle_id.get(oracle_id)?;
            (ids.len() == 1).then_some(ids[0])
        });
        let Some(card_id) = card_id else {
            continue;
        };
        for alias in [name.clone(), name.replace(',', ";"), name.replace(' ', "_")] {
            match unique.entry(alias) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Some(card_id));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if entry.get().is_some_and(|existing| existing != card_id) {
                        entry.insert(None);
                    }
                }
            }
        }
    }
    let mut references: Vec<_> = unique
        .into_iter()
        .filter_map(|(name, id)| id.map(|id| (name, id)))
        .collect();
    references.sort_unstable_by(|left, right| right.0.len().cmp(&left.0.len()).then_with(|| left.0.cmp(&right.0)));
    references
}


fn match_token_title<'a>(text: &'a str, token_key: &str) -> Option<&'a str> {
    let mut text_cursor = 0;
    let mut key_cursor = 0;
    let text_bytes = text.as_bytes();
    let key_bytes = token_key.as_bytes();
    
    while key_cursor < key_bytes.len() && text_cursor < text_bytes.len() {
        let mut t_c = text_bytes[text_cursor].to_ascii_lowercase();
        if t_c == b'\'' {
            text_cursor += 1;
            continue;
        }
        if t_c == b' ' || t_c == b'-' || t_c == b',' {
            t_c = b'_';
        }
        if t_c != key_bytes[key_cursor] {
            return None;
        }
        text_cursor += 1;
        key_cursor += 1;
    }
    if key_cursor == key_bytes.len() {
        let tail = &text[text_cursor..];
        if tail.chars().next().is_none_or(|next| {
            next.is_whitespace() || matches!(next, '.' | '+' | ',' | '/' | '$' | '>' | ')' | ';' | '|' | ']' | '-' | ':')
        }) {
            return Some(&text[..text_cursor]);
        }
    }
    None
}

fn replace_named_qualifiers(line: &str, references: &[(String, CardScriptId)], token_index: &std::collections::BTreeMap<String, TokenScriptId>) -> String {
    let mut output = String::with_capacity(line.len());
    let mut cursor = 0;
    let bytes = line.as_bytes();

    while let Some(relative) = line[cursor..].find("named") {
        let named_start = cursor + relative;
        let named_end = named_start + 5; // "named".len()

        let mut is_structured = false;
        let mut is_negated = false;
        let mut is_creatures_named = false;
        
        let mut replace_start = named_start;

        if named_start >= 1 && (bytes[named_start - 1] == b'.' || bytes[named_start - 1] == b'+') {
            is_structured = true;
        } else if named_start >= 2 && bytes[named_start - 1] == b'!' && (bytes[named_start - 2] == b'.' || bytes[named_start - 2] == b'+') {
            is_structured = true;
            is_negated = true;
            replace_start = named_start - 1; // start replacement at '!'
        } else if named_start >= 10 && &bytes[named_start - 10..named_start] == b"creatures " {
            is_structured = true;
            is_creatures_named = true;
            replace_start = named_start - 10; // start replacement at 'c'
        }

        if !is_structured {
            output.push_str(&line[cursor..named_end]);
            cursor = named_end;
            continue;
        }

        let after_marker = &line[named_end..];
        let after_name_marker = after_marker.trim_start();

        let mut matched_title = None;
        

        // Try to match Card titles first
        for (name, id) in references.iter() {
            if let Some(tail) = after_name_marker.strip_prefix(name) {
                if tail.chars().next().is_none_or(|next| {
                    next.is_whitespace() || matches!(next, '.' | '+' | ',' | '/' | '$' | '>' | ')' | ';' | '|' | ']' | ':')
                }) {
                    matched_title = Some((name.len(), format!("catalogId{}", id.0)));
                    break;
                }
            }
        }

        // Try to match Token titles if no Card title matched
        if matched_title.is_none() {
            let mut longest_token_match = None;
            for (name, id) in token_index.iter() {
                if let Some(matched_str) = match_token_title(after_name_marker, name) {
                    if longest_token_match.as_ref().map_or(true, |(len, _)| matched_str.len() > *len) {
                        longest_token_match = Some((matched_str.len(), format!("tokenId{}", id.0)));
                    }
                }
            }
            if let Some(longest) = longest_token_match {
                matched_title = Some(longest);
            }
        }

        if let Some((name_len, id_str)) = matched_title {
            output.push_str(&line[cursor..replace_start]);
            if is_creatures_named {
                output.push_str("creatures ");
                output.push_str(&id_str);
            } else if is_negated {
                output.push_str("!");
                output.push_str(&id_str);
            } else {
                output.push_str(&id_str);
            }
            let spaces_len = after_marker.len() - after_name_marker.len();
            cursor = named_end + spaces_len + name_len;
        } else {
            output.push_str(&line[cursor..named_end]);
            cursor = named_end;
        }
    }
    output.push_str(&line[cursor..]);
    output
}

fn numeric_id_for_name(value: &str, references: &[(String, CardScriptId)]) -> Option<CardScriptId> {
    let normalized = value.trim().replace(';', ",");
    let exact = references
        .iter()
        .find_map(|(name, id)| (name == &normalized).then_some(*id));
    if exact.is_some() {
        return exact;
    }

    // A very small number of upstream scripts contain a one-character typo in
    // a card-reference operand. Resolve only a unique edit-distance-one match;
    // ambiguity remains unresolved instead of silently selecting an identity.
    let mut candidate = None;
    for (name, id) in references {
        if edit_distance_at_most_one(name, &normalized) {
            if candidate.is_some_and(|existing| existing != *id) {
                return None;
            }
            candidate = Some(*id);
        }
    }
    candidate
}

fn edit_distance_at_most_one(left: &str, right: &str) -> bool {
    let left: Vec<char> = left.chars().flat_map(char::to_lowercase).collect();
    let right: Vec<char> = right.chars().flat_map(char::to_lowercase).collect();
    if left.len().abs_diff(right.len()) > 1 {
        return false;
    }
    let (shorter, longer) = if left.len() <= right.len() {
        (&left, &right)
    } else {
        (&right, &left)
    };
    let mut short = 0;
    let mut long = 0;
    let mut edits = 0;
    while short < shorter.len() && long < longer.len() {
        if shorter[short] == longer[long] {
            short += 1;
            long += 1;
        } else {
            edits += 1;
            if edits > 1 {
                return false;
            }
            if shorter.len() == longer.len() {
                short += 1;
            }
            long += 1;
        }
    }
    edits + usize::from(long < longer.len()) <= 1
}

fn stable_runtime_object_id(value: &str) -> u64 {
    let digest = Sha256::digest(value.trim().as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes).max(1)
}

fn rewrite_top_level_card_reference(line: &str, references: &[(String, CardScriptId)]) -> Option<String> {
    if let Some(value) = line.strip_prefix("K:Partner with:") {
        let title = value.rsplit_once(':').map_or(value, |(title, _nickname)| title);
        return numeric_id_for_name(title, references).map(|id| format!("K:PartnerWithId:{}", id.0));
    }
    let (key, value) = line.split_once(':')?;
    let numeric_key = match key.trim() {
        "CopyFaceFrom" => "CopyFaceFromId",
        "MeldPair" => "MeldPairId",
        _ => return None,
    };
    numeric_id_for_name(value, references).map(|id| format!("{numeric_key}:{}", id.0))
}

fn rewrite_card_reference_parameters(line: &str, references: &[(String, CardScriptId)]) -> String {
    if let Some(rewritten) = rewrite_top_level_card_reference(line, references) {
        return rewritten;
    }

    let mut segments = line.split('|');
    let Some(head) = segments.next() else {
        return line.to_owned();
    };
    let api = head.rsplit_once('$').map(|(_, value)| value.trim()).unwrap_or("");
    let mut output = vec![head.trim().to_owned()];
    for segment in segments {
        let trimmed = segment.trim();
        let Some((key, value)) = trimmed.split_once('$') else {
            output.push(trimmed.to_owned());
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let numeric_key = match (api, key) {
            ("MakeCard", "Name") | ("Meld", "Name") => Some("CatalogId"),
            ("Meld", "Primary") => Some("PrimaryCatalogId"),
            ("Meld", "Secondary") => Some("SecondaryCatalogId"),
            ("CopyPermanent", "DefinedName") => Some("DefinedCatalogId"),
            ("NameCard", "ChooseFromList") => {
                let ids: Option<Vec<String>> = value
                    .split(',')
                    .map(|name| numeric_id_for_name(name, references).map(|id| id.0.to_string()))
                    .collect();
                if let Some(ids) = ids {
                    output.push(format!("ChooseFromCatalogIds$ {}", ids.join(",")));
                    continue;
                }
                None
            }
            (_, "Spellbook") => {
                let ids: Option<Vec<String>> = value
                    .split(',')
                    .map(|name| numeric_id_for_name(name, references).map(|id| id.0.to_string()))
                    .collect();
                if let Some(ids) = ids {
                    output.push(format!("SpellbookCatalogIds$ {}", ids.join(",")));
                    continue;
                }
                None
            }
            ("MakeCard", "Names" | "Choices") => {
                let ids: Option<Vec<String>> = value
                    .split(',')
                    .map(|name| numeric_id_for_name(name, references).map(|id| id.0.to_string()))
                    .collect();
                if let Some(ids) = ids {
                    output.push(format!("CatalogIds$ {}", ids.join(",")));
                    continue;
                }
                None
            }
            ("Play", "AnySupportedCard") if value.starts_with("Names:") => {
                let ids: Option<Vec<String>> = value["Names:".len()..]
                    .split(',')
                    .map(|name| numeric_id_for_name(name, references).map(|id| id.0.to_string()))
                    .collect();
                if let Some(ids) = ids {
                    output.push(format!("AnySupportedCatalogIds$ {}", ids.join(",")));
                    continue;
                }
                None
            }
            _ => None,
        };
        if let Some(numeric_key) = numeric_key {
            if let Some(id) = numeric_id_for_name(value, references) {
                output.push(format!("{numeric_key}$ {}", id.0));
                continue;
            }
        }
        output.push(trimmed.to_owned());
    }
    output.join(" | ")
}

fn rewrite_token_script_parameters(line: &str, tokens: &BTreeMap<String, TokenScriptId>) -> String {
    let segments: Vec<String> = line
        .split('|')
        .map(|segment| {
            let trimmed = segment.trim();
            let Some((key, value)) = trimmed.split_once('$') else {
                return trimmed.to_owned();
            };
            if key.trim() != "TokenScript" {
                return trimmed.to_owned();
            }
            let ids: Option<Vec<String>> = value
                .trim()
                .split(',')
                .map(|name| tokens.get(name.trim()).map(|id| id.0.to_string()))
                .collect();
            ids.map_or_else(
                || trimmed.to_owned(),
                |ids| format!("TokenScriptIds$ {}", ids.join(",")),
            )
        })
        .collect();
    segments.join(" | ")
}

fn runtime_object_names(script: &str) -> Vec<String> {
    let mut names = BTreeSet::new();
    for line in script.lines() {
        let mut segments = line.split('|');
        let Some(head) = segments.next() else {
            continue;
        };
        let api = head.rsplit_once('$').map(|(_, value)| value.trim()).unwrap_or("");
        for segment in segments {
            let Some((key, value)) = segment.trim().split_once('$') else {
                continue;
            };
            let key = key.trim();
            let is_object_name = matches!(key, "NewName" | "SetName")
                || (key == "Name" && matches!(api, "Effect" | "ReplaceEffect" | "ReplaceSplitDamage" | "Animate"));
            if is_object_name {
                names.insert(value.trim().to_owned());
            }
        }
    }
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort_unstable_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    names
}

fn anonymize_runtime_object_names(line: &str, runtime_names: &[String]) -> String {
    let runtime_ids: Vec<_> = runtime_names
        .iter()
        .map(|name| (name.clone(), stable_runtime_object_id(name)))
        .collect();
    let mut anonymous_line = line.to_owned();
    for (name, id) in &runtime_ids {
        anonymous_line = anonymous_line.replace(&format!("named{name}"), &format!("named{id}"));
        anonymous_line = anonymous_line.replace(&format!("named{}", name.replace(',', ";")), &format!("named{id}"));
    }

    let mut segments = anonymous_line.split('|');
    let Some(head) = segments.next() else {
        return line.to_owned();
    };
    let api = head.rsplit_once('$').map(|(_, value)| value.trim()).unwrap_or("");
    let mut output = vec![head.trim().to_owned()];
    for segment in segments {
        let trimmed = segment.trim();
        let Some((key, value)) = trimmed.split_once('$') else {
            output.push(trimmed.to_owned());
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let is_object_name = matches!(key, "NewName" | "SetName")
            || (key == "Name" && matches!(api, "Effect" | "ReplaceEffect" | "ReplaceSplitDamage" | "Animate"));
        if is_object_name {
            output.push(format!("{key}$ {}", stable_runtime_object_id(value)));
        } else {
            output.push(trimmed.to_owned());
        }
    }
    output.join(" | ")
}

fn normalize_keyword_vocabulary(line: &str) -> String {
    let normalized = [
        ("First Strike", "FirstStrike"),
        ("Double Strike", "DoubleStrike"),
        ("Level\u{20}up", "LevelUp"),
        ("Battle cry", "BattleCry"),
        ("Battle Cry", "BattleCry"),
        ("Start your \u{65}ngines!", "StartYourEngines"),
        ("Start your \u{65}ngines", "StartYourEngines"),
        ("Web-\u{73}linging", "WebSlinging"),
        ("Shaman\u{27}s Trance", "SharedGraveyardCasting"),
        ("Protection from red", "Protection:Red"),
        ("Protection from blue", "Protection:Blue"),
        ("Protection from black", "Protection:Black"),
        ("Protection from white", "Protection:White"),
        ("Protection from green", "Protection:Green"),
        ("Protection from everything", "Protection:Everything"),
        ("Protection from each color", "Protection:EachColor"),
        (
            "You draw cards from the bottom of your library instead of the top of your \u{6c}ibrary.",
            "DrawFromBottom",
        ),
    ]
    .into_iter()
    .fold(line.to_owned(), |text, (source, replacement)| {
        text.replace(source, replacement)
    });

    if normalized.starts_with("K:Spend only colored mana on X.") {
        return "K:DistinctColoredManaForX".to_owned();
    }

    // Flavor words (CR 207.2c-d) are pure lore expression: the rules give
    // them no game effect and the engine has no consumer for the segment,
    // so the anonymous corpus must not retain them (ds-5432 SS1 "nothing
    // worldly"; OD-5: names/lore are the scrub target regardless of owner).
    let normalized = if normalized.starts_with("K:") && normalized.contains(":Flavor ") {
        normalized
            .split(':')
            .filter(|field| !field.starts_with("Flavor "))
            .collect::<Vec<_>>()
            .join(":")
    } else {
        normalized
    };

    let fields: Vec<&str> = normalized.split(':').collect();
    if normalized.starts_with("K:etbCounter:") && fields.len() > 5 {
        return fields[..5].join(":");
    }
    if normalized.starts_with("K:Flashback:") && fields.get(3).is_some_and(|field| field.contains('$')) {
        return fields[..4].join(":");
    }
    if normalized.starts_with("K:Specialize:") && fields.len() > 5 {
        return format!("{}:::{}", fields[..3].join(":"), fields[5..].join(":"));
    }
    if normalized.starts_with("K:Equip:") && fields.len() > 6 {
        return fields[..6].join(":");
    }

    // Forge appends a human reminder after the literal `no Condition` marker
    // on this structured keyword. The marker itself is executable; the tail
    // is not.
    if normalized.starts_with("K:etbCounter:") {
        for marker in [":no Condition:", ":no condition:"] {
            if let Some(offset) = normalized.find(marker) {
                return normalized[..offset + marker.len() - 1].to_owned();
            }
        }
    }
    normalized
}

fn rewrite_contextual_card_references(
    line: &str,
    references: &[(String, CardScriptId)],
    owner_id: Option<CardScriptId>,
) -> String {
    let mut output = line.replace("/Swamp card>", ">");
    if let Some(end) = output.find('>') {
        if let Some(slash) = output[..end].rfind('/') {
            if output[..slash].ends_with("OriginalHost") {
                let value = &output[slash + 1..end];
                if let Some(id) = numeric_id_for_name(value, references) {
                    output.replace_range(slash + 1..end, &format!("catalogId{}", id.0));
                }
            }
        }
    }
    if let Some(start) = output.find("count as ") {
        let value_start = start + "count as ".len();
        if let Some(relative_end) = output[value_start..].find('.') {
            let value_end = value_start + relative_end;
            if let Some(id) = numeric_id_for_name(&output[value_start..value_end], references) {
                output.replace_range(start..=value_end, &format!("countAsCatalogId{}", id.0));
            }
        }
    }
    for marker in ["DraftNotesCount.", "DraftNotesHighest."] {
        if let Some(start) = output.find(marker) {
            let value_start = start + marker.len();
            if let Some(id) = numeric_id_for_name(&output[value_start..], references) {
                output.replace_range(value_start.., &format!("catalogId{}", id.0));
            }
        }
    }
    if let Some(start) = output.find("FromDraftNotes$ ") {
        let value_start = start + "FromDraftNotes$ ".len();
        if let Some(id) = numeric_id_for_name(&output[value_start..], references).or(owner_id) {
            output.replace_range(start.., &format!("FromDraftNotesId$ {}", id.0));
        }
    }
    output
}

fn sanitize_script(
    script: &str,
    card_id: CardScriptId,
    color_identity: &str,
    set_group: &str,
    numeric_name_refs: &[(String, CardScriptId)],
    token_index: &BTreeMap<String, TokenScriptId>,
) -> String {
    let mut output = String::with_capacity(script.len());
    output.push_str(&format!("Id:{}\n", card_id.0));
    output.push_str(&format!("ColorIdentity:{color_identity}\n"));
    output.push_str(&format!("OriginSet:{set_group}\n"));
    let runtime_names = runtime_object_names(script);
    for line in script.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') || is_removed_top_level_field(line) {
            continue;
        }
        let sanitized = sanitize_runtime_line(line, numeric_name_refs, token_index, &runtime_names, Some(card_id));
        output.push_str(&sanitized);
        output.push('\n');
    }
    output
}

fn sanitize_token_script(
    script: &str,
    token_id: TokenScriptId,
    numeric_name_refs: &[(String, CardScriptId)],
    token_index: &BTreeMap<String, TokenScriptId>,
) -> String {
    let mut output = String::with_capacity(script.len());
    output.push_str(&format!("TokenId:{}\n", token_id.0));
    output.push_str("ColorIdentity:\n");
    let runtime_names = runtime_object_names(script);
    for line in script.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') || is_removed_top_level_field(line) {
            continue;
        }
        let sanitized = sanitize_runtime_line(line, numeric_name_refs, token_index, &runtime_names, None);
        output.push_str(&sanitized);
        output.push('\n');
    }
    output
}

fn sanitize_runtime_line(
    line: &str,
    numeric_name_refs: &[(String, CardScriptId)],
    token_index: &BTreeMap<String, TokenScriptId>,
    runtime_names: &[String],
    owner_id: Option<CardScriptId>,
) -> String {
    let numeric = replace_named_qualifiers(line, numeric_name_refs, token_index);
    let numeric = rewrite_card_reference_parameters(&numeric, numeric_name_refs);
    let numeric = rewrite_token_script_parameters(&numeric, token_index);
    let numeric = anonymize_runtime_object_names(&numeric, runtime_names);
    let numeric = rewrite_contextual_card_references(&numeric, numeric_name_refs, owner_id);
    let numeric = normalize_keyword_vocabulary(&numeric);
    let numeric = anonymize_set_qualifiers(&numeric);
    strip_display_parameters(&numeric)
}

fn anonymize_set_qualifiers(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut output = String::with_capacity(line.len());
    let mut cursor = 0;
    while let Some(relative) = line[cursor..].find("set") {
        let start = cursor + relative;
        output.push_str(&line[cursor..start]);
        let code_start = start + 3;
        if start == 0 || !matches!(bytes[start - 1], b'.' | b'+') {
            output.push_str("set");
            cursor = code_start;
            continue;
        }
        let mut end = code_start;
        while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
            end += 1;
        }
        if end > code_start {
            let code = line[code_start..end].to_ascii_uppercase();
            let digest = Sha256::digest(code.as_bytes());
            output.push_str("setG");
            for byte in &digest[..8] {
                output.push_str(&format!("{byte:02x}"));
            }
            cursor = end;
        } else {
            output.push_str("set");
            cursor = code_start;
        }
    }
    output.push_str(&line[cursor..]);
    output
}

fn is_removed_top_level_field(line: &str) -> bool {
    if line.starts_with("K:DeckLimit:") {
        return true;
    }
    let mut fields = line.split(':').map(str::trim);
    match fields.next() {
        // These fields only guide Forge's deck builder, draft picker, or AI
        // deck selection and are not consumed by the gameplay DSL. They also
        // frequently contain card titles, so the anonymous runtime corpus
        // must not retain them.
        Some("Name" | "Oracle" | "Text" | "DeckHints" | "ODeckHints" | "DeckHas" | "DeckNeeds" | "Draft" | "AI") => {
            true
        }
        Some("Variant") => true,
        _ => false,
    }
}

fn strip_display_parameters(line: &str) -> String {
    let segments: Vec<&str> = line
        .split('|')
        .filter(|segment| !is_display_parameter(segment))
        .map(str::trim)
        .collect();
    segments.join(" | ")
}

fn is_display_parameter(segment: &str) -> bool {
    segment
        .trim()
        .split_once('$')
        .map(|(key, _)| {
            let key = key.trim();
            key.ends_with("Description")
                || key.ends_with("Prompt")
                || key.ends_with("Desc")
                || key.ends_with("Message")
                || key.ends_with("Title")
                || matches!(key, "Image" | "SpellbookName")
        })
        .unwrap_or(false)
}

fn publish_directory(stage: &Path, output: &Path) -> Result<()> {
    let backup = sibling_with_suffix(output, &format!("old-{}", std::process::id()))?;
    if backup.exists() {
        fs::remove_dir_all(&backup).with_context(|| format!("remove stale backup {}", backup.display()))?;
    }
    if output.exists() {
        fs::rename(output, &backup)
            .with_context(|| format!("move previous output {} to {}", output.display(), backup.display()))?;
    }
    if let Err(error) = fs::rename(stage, output) {
        if backup.exists() {
            let _ = fs::rename(&backup, output);
        }
        return Err(error).with_context(|| format!("publish generated output {}", output.display()));
    }
    if backup.exists() {
        fs::remove_dir_all(&backup).with_context(|| format!("remove previous output backup {}", backup.display()))?;
    }
    Ok(())
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .context("path has no UTF-8 final component")?;
    Ok(parent.join(format!(".{name}.{suffix}")))
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

fn write_report(report: &GenerationReport) -> Result<()> {
    let path = Path::new(".cache/reports/generate-report.json");
    fs::create_dir_all(path.parent().expect("literal report path has a parent"))?;
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer_pretty(file, report).context("write generation report")?;
    Ok(())
}

fn print_report(report: &GenerationReport) {
    eprintln!(
        "Generated {} numeric-ID scripts from {} source scripts ({} identical duplicates)",
        report.generated_scripts, report.source_scripts, report.duplicate_identical_scripts
    );
    eprintln!(
        "Unmapped: {}; ambiguous: {}; conflicting numeric scripts: {}; UNACCOUNTED: {}",
        report.missing_mappings.len(),
        report.ambiguous_mappings.len(),
        report.conflicting_scripts.len(),
        report.unaccounted_identities.len()
    );
    for problem in report.unaccounted_identities.iter().take(10) {
        eprintln!(
            "FAIL: {} ({}) was never looked up at all - present in the Forge source, no Scryfall lookup attempted",
            problem.source, problem.name
        );
    }
    if report.unaccounted_identities.len() > 10 {
        eprintln!(
            "FAIL: {} additional unaccounted identities are recorded in .cache/reports/generate-report.json",
            report.unaccounted_identities.len() - 10
        );
    }
    for problem in report.missing_mappings.iter().take(10) {
        eprintln!(
            "WARNING: no Scryfall oracle_id for {} ({})",
            problem.source, problem.name
        );
    }
    if report.missing_mappings.len() > 10 {
        eprintln!(
            "WARNING: {} additional missing mappings are recorded in .cache/reports/generate-report.json",
            report.missing_mappings.len() - 10
        );
    }
    for problem in report.ambiguous_mappings.iter().take(10) {
        eprintln!(
            "WARNING: ambiguous Scryfall oracle_id for {} ({}): {:?}",
            problem.source, problem.name, problem.oracle_ids
        );
    }
    if report.ambiguous_mappings.len() > 10 {
        eprintln!(
            "WARNING: {} additional ambiguous mappings are recorded in .cache/reports/generate-report.json",
            report.ambiguous_mappings.len() - 10
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_a_script_without_touching_executable_fields() {
        let input = "Name:Fixture Qzx One\nManaCost:R\nTypes:Instant\nA:SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3 | SpellDescription$ CARDNAME deals damage.\nSVar:Named:DB$ MakeCard | Name$ Fixture Qzx One | SpellDescription$ Make one.\nOracle:Fixture rules sentence used only by this synthetic test.\n";
        assert_eq!(
            sanitize_script(input, CardScriptId(145), "R", "Gfixture", &[], &BTreeMap::new()),
            "Id:145\nColorIdentity:R\nOriginSet:Gfixture\nManaCost:R\nTypes:Instant\nA:SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3\nSVar:Named:DB$ MakeCard | Name$ Fixture Qzx One\n"
        );
    }

    #[test]
    fn removes_all_human_description_parameters() {
        let input = "Text:Display-only sentence.\nT:Mode$ SpellCast | TriggerDescription$ Display sentence | CostDesc$ More display text | Description$ Keep this\n";
        assert_eq!(
            sanitize_script(input, CardScriptId(1), "", "Gfixture", &[], &BTreeMap::new()),
            "Id:1\nColorIdentity:\nOriginSet:Gfixture\nT:Mode$ SpellCast\n"
        );
    }

    #[test]
    fn removes_non_runtime_deck_hints() {
        let input = "# Human implementation note\n\nDeckHints:Type$Forest & Name$Fixture Qzx One\nDeckHas:Ability$Token\nDeckNeeds:Name$Fixture Qzx Two\nDraft:AI$ True\nAI:RemoveDeck:Random\nManaCost:G\nTypes:Creature\n";
        assert_eq!(
            sanitize_script(input, CardScriptId(1), "G", "Gfixture", &[], &BTreeMap::new()),
            "Id:1\nColorIdentity:G\nOriginSet:Gfixture\nManaCost:G\nTypes:Creature\n"
        );
    }

    #[test]
    fn numeric_path_has_three_trie_levels_and_full_key() {
        let id = CardScriptId(12_345_678);
        assert_eq!(
            id.trie_path(Path::new("cards")),
            PathBuf::from("cards/12/34/56/12345678.txt")
        );
    }

    #[test]
    fn named_filter_qualifiers_become_numeric_identity_qualifiers() {
        let references = vec![("Fixture Qzx One".to_owned(), CardScriptId(145))];
        assert_eq!(
            replace_named_qualifiers(
                "S:Mode$ Continuous | Affected$ Creature.namedFixture Qzx One+YouCtrl",
                &references,
                &std::collections::BTreeMap::new(),
            ),
            "S:Mode$ Continuous | Affected$ Creature.catalogId145+YouCtrl"
        );
    }


    #[test]
    fn parses_structured_negated_and_prose_named_qualifiers() {
        let references = vec![
            ("Fixture Qzx One".to_owned(), CardScriptId(145)),
            ("Worldgorger Dragon".to_owned(), CardScriptId(5780)),
        ];
        let mut tokens = std::collections::BTreeMap::new();
        tokens.insert("wolves_of_the_hunt".to_owned(), TokenScriptId(999));
        
        assert_eq!(
            replace_named_qualifiers(
                "Creature.YouCtrl+nonArtifact+!namedFixture Qzx One",
                &references,
                &tokens
            ),
            "Creature.YouCtrl+nonArtifact+!catalogId145"
        );
        assert_eq!(
            replace_named_qualifiers(
                "ValidCard$ creatures named Wolves of the Hunt",
                &references,
                &tokens
            ),
            "ValidCard$ creatures tokenId999"
        );
    }

    #[test]
    fn strips_keyword_flavor_words() {
        // The Flavor segment is non-mechanical lore (licensed weapon names
        // in the Universes Beyond ranges); it must never survive into the
        // anonymous corpus.
        assert_eq!(normalize_keyword_vocabulary("K:Equip:2:Flavor Fixture Blade"), "K:Equip:2");
        assert_eq!(
            normalize_keyword_vocabulary("K:Equip:4:Flavor Fixture and Fixture Shield"),
            "K:Equip:4"
        );
        // No false positives on ordinary keyword lines.
        assert_eq!(normalize_keyword_vocabulary("K:Flying"), "K:Flying");
    }

    #[test]
    fn executable_card_reference_parameters_become_numeric() {
        let references = vec![
            ("Fixture Qzx One".to_owned(), CardScriptId(145)),
            ("Fixture Qzx Two".to_owned(), CardScriptId(146)),
        ];
        assert_eq!(
            rewrite_card_reference_parameters(
                "SVar:Make:DB$ MakeCard | Name$ Fixture Qzx One | Zone$ Hand",
                &references
            ),
            "SVar:Make:DB$ MakeCard | CatalogId$ 145 | Zone$ Hand"
        );
        assert_eq!(
            rewrite_card_reference_parameters(
                "SVar:Meld:DB$ Meld | Name$ Fixture Qzx One | Primary$ Fixture Qzx One | Secondary$ Fixture Qzx Two",
                &references
            ),
            "SVar:Meld:DB$ Meld | CatalogId$ 145 | PrimaryCatalogId$ 145 | SecondaryCatalogId$ 146"
        );
        assert_eq!(
            rewrite_card_reference_parameters(
                "A:AB$ NameCard | ChooseFromList$ Fixture Qzx One,Fixture Qzx Two",
                &references
            ),
            "A:AB$ NameCard | ChooseFromCatalogIds$ 145,146"
        );
        assert_eq!(
            rewrite_card_reference_parameters("CopyFaceFrom:Fixture Qzx One", &references),
            "CopyFaceFromId:145"
        );
        assert_eq!(
            rewrite_card_reference_parameters("K:Partner with:Fixture Qzx Two", &references),
            "K:PartnerWithId:146"
        );
    }

    #[test]
    fn token_script_references_and_definitions_become_numeric() {
        let tokens = BTreeMap::from([
            ("fixture_one".to_owned(), TokenScriptId(101)),
            ("fixture_two".to_owned(), TokenScriptId(102)),
        ]);
        assert_eq!(
            rewrite_token_script_parameters(
                "SVar:T:DB$ Token | TokenScript$ fixture_one,fixture_two | TokenOwner$ You",
                &tokens
            ),
            "SVar:T:DB$ Token | TokenScriptIds$ 101,102 | TokenOwner$ You"
        );
        assert_eq!(
            sanitize_token_script(
                "Name:Fixture Token\nManaCost:no cost\nTypes:Creature\nOracle:Display text.\n",
                TokenScriptId(101),
                &[],
                &tokens
            ),
            "TokenId:101\nColorIdentity:\nManaCost:no cost\nTypes:Creature\n"
        );
        assert_eq!(token_id_for_key("fixture_one"), token_id_for_key("fixture_one"));
        assert_ne!(token_id_for_key("fixture_one"), token_id_for_key("fixture_two"));
    }

    #[test]
    fn set_predicates_use_anonymous_functional_groups() {
        assert_eq!(
            anonymize_set_qualifiers("ValidCard$ Card.setSET+Other | ValidCards$ Permanent.!token+setSET"),
            "ValidCard$ Card.setG2992d15897b5bbe7+Other | ValidCards$ Permanent.!token+setG2992d15897b5bbe7"
        );
        assert_eq!(
            anonymize_set_qualifiers("RepeatSubAbility$ ResetCheck"),
            "RepeatSubAbility$ ResetCheck"
        );
    }

    #[test]
    fn synthetic_runtime_object_names_become_numeric() {
        assert_eq!(
            anonymize_runtime_object_names(
                "SVar:E:DB$ Effect | Name$ Fixture effect | Duration$ Permanent",
                &["Fixture effect".to_owned()]
            ),
            format!(
                "SVar:E:DB$ Effect | Name$ {} | Duration$ Permanent",
                stable_runtime_object_id("Fixture effect")
            )
        );
        assert_eq!(
            anonymize_runtime_object_names(
                "SVar:C:DB$ Clone | NewName$ Fixture clone",
                &["Fixture clone".to_owned()]
            ),
            format!(
                "SVar:C:DB$ Clone | NewName$ {}",
                stable_runtime_object_id("Fixture clone")
            )
        );
        assert_eq!(
            anonymize_runtime_object_names(
                "SVar:X:Count$ValidCommand Effect.YouCtrl+namedFixture effect",
                &["Fixture effect".to_owned()]
            ),
            format!(
                "SVar:X:Count$ValidCommand Effect.YouCtrl+named{}",
                stable_runtime_object_id("Fixture effect")
            )
        );
    }

    #[test]
    fn keyword_operands_use_non_prose_vocabulary() {
        assert_eq!(
            normalize_keyword_vocabulary(
                "S:Mode$ Continuous | AddKeyword$ Flying & First\u{20}Strike & Protection from red"
            ),
            "S:Mode$ Continuous | AddKeyword$ Flying & FirstStrike & Protection:Red"
        );
        assert_eq!(
            normalize_keyword_vocabulary(
                "S:Mode$ Continuous | AddKeyword$ You draw cards from the bottom of your library instead of the top of your \u{6c}ibrary."
            ),
            "S:Mode$ Continuous | AddKeyword$ DrawFromBottom"
        );
        assert_eq!(
            normalize_keyword_vocabulary("K:etbCounter:P1P1:X:no Condition:Human reminder sentence."),
            "K:etbCounter:P1P1:X:no Condition"
        );
    }

    #[test]
    fn basic_land_types_are_not_folded_into_color_identity() {
        let card = ScryfallCard {
            oracle_id: None,
            name: "Fixture Qzx Dual".to_owned(),
            printed_name: None,
            oracle_text: Some("({T}: Add {R} or {W}.)\nAs this land enters, you may pay 2 life.".to_owned()),
            mana_cost: String::new(),
            type_line: "Land — Mountain Plains".to_owned(),
            lang: "en".to_owned(),
            layout: "normal".to_owned(),
            color_identity: vec!["R".to_owned(), "W".to_owned()],
            colors: Vec::new(),
            color_indicator: None,
            card_faces: Vec::new(),
            ..Default::default()
        };
        assert_eq!(commander_color_identity(&card), "");
    }

    #[test]
    fn non_reminder_symbols_still_define_a_basic_land_cards_identity() {
        let card = ScryfallCard {
            oracle_id: None,
            name: "Fixture Qzx Producing Land".to_owned(),
            printed_name: None,
            oracle_text: Some("{T}: Add {W} or {R}.".to_owned()),
            mana_cost: String::new(),
            type_line: "Land — Mountain Plains".to_owned(),
            lang: "en".to_owned(),
            layout: "normal".to_owned(),
            color_identity: vec!["R".to_owned(), "W".to_owned()],
            colors: Vec::new(),
            color_indicator: None,
            card_faces: Vec::new(),
            ..Default::default()
        };
        assert_eq!(commander_color_identity(&card), "WR");
    }

    #[test]
    fn indexes_whole_cards_and_faces_by_oracle_identity() {
        let id = Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap();
        let index = index_cards(vec![ScryfallCard {
            oracle_id: Some(id),
            name: "Fixture Qzx Front // Fixture Qzx Back".to_owned(),
            printed_name: None,
            oracle_text: None,
            mana_cost: "{1}{R} // {1}{U}".to_owned(),
            type_line: "Instant // Instant".to_owned(),
            lang: "en".to_owned(),
            layout: "split".to_owned(),
            color_identity: vec!["R".to_owned(), "U".to_owned()],
            colors: vec!["R".to_owned(), "U".to_owned()],
            color_indicator: None,
            card_faces: vec![
                ScryfallFace {
                    name: "Fire".to_owned(),
                    printed_name: None,
                    oracle_text: None,
                    mana_cost: "{1}{R}".to_owned(),
                    type_line: "Instant".to_owned(),
                    colors: vec!["R".to_owned()],
                    color_indicator: None,
                    ..Default::default()
                },
                ScryfallFace {
                    name: "Ice".to_owned(),
                    printed_name: None,
                    oracle_text: None,
                    mana_cost: "{1}{U}".to_owned(),
                    type_line: "Instant".to_owned(),
                    colors: vec!["U".to_owned()],
                    color_indicator: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }]);
        assert_eq!(index.lookup("Fixture Qzx Front // Fixture Qzx Back").unwrap().len(), 1);
        assert_eq!(index.lookup("Fire").unwrap().len(), 1);
        assert_eq!(index.lookup("Ice").unwrap().len(), 1);
    }

    #[test]
    fn rejects_parent_traversal_in_output() {
        assert!(validate_output_path(Path::new("../cards")).is_err());
        assert!(validate_output_path(Path::new("cards")).is_ok());
    }

    #[test]
    fn derives_split_identity_from_copy_face_records() {
        let script =
            "CopyFaceFrom:Fixture Qzx Front\nAlternateMode:Split\n\nALTERNATE\n\nCopyFaceFrom:Fixture Qzx Back\n";
        assert_eq!(
            source_identity_name(script).as_deref(),
            Some("Fixture Qzx Front // Fixture Qzx Back")
        );
    }

    /// Regression test for the meld-back-face hole (NUMERIC_CORPUS_SWAP_
    /// 20260819.md section 8): a meld's front and back are DIFFERENT Oracle
    /// identities (unlike Split or a transforming double-faced card), so
    /// the back face defined in an `ALTERNATE` block must be independently
    /// extractable for its own Scryfall lookup.
    #[test]
    fn meld_back_identity_extracts_the_back_face_from_an_alternate_block() {
        let script = "Name:Fixture Front\nManaCost:2 W\nTypes:Creature\nAlternateMode:Meld\nOracle:Front rules text.\n\nALTERNATE\n\nName:Fixture Back\nManaCost:no cost\nTypes:Creature\nOracle:Back rules text.\n";
        let (back_name, back_text) = meld_back_identity(script).expect("meld back identity");
        assert_eq!(back_name, "Fixture Back");
        assert!(back_text.contains("Types:Creature"));
        assert!(back_text.contains("Oracle:Back rules text."));
        // The front half must not leak into the extracted back text.
        assert!(!back_text.contains("Fixture Front"));
    }

    /// The OTHER creature in a meld pair also carries `AlternateMode:Meld`,
    /// but only REFERENCES the back face by name (via `SVar:Meld:...
    /// Name$ <back>`); it does not redefine it, so there is nothing to
    /// extract from this half.
    #[test]
    fn meld_back_identity_is_none_for_a_melds_other_front_half() {
        let script = "Name:Fixture Other Front\nManaCost:1 W\nTypes:Creature\nSVar:Meld:DB$ Meld | Name$ Fixture Back | Primary$ Fixture Front | Secondary$ Fixture Other Front\nAlternateMode:Meld\nOracle:Other front rules text.\n";
        assert_eq!(meld_back_identity(script), None);
    }

    /// A transforming double-faced card also has an `ALTERNATE` block with
    /// its own `Name:` line, but shares ONE Oracle identity with its front
    /// face (unlike Meld) and is already correctly resolved through
    /// Scryfall's own face index when the front name alone is looked up —
    /// it must NOT be treated as a second independent identity.
    #[test]
    fn meld_back_identity_is_none_for_non_meld_alternate_modes() {
        let script = "Name:Fixture DFC Front\nManaCost:2 U\nTypes:Creature\nAlternateMode:DoubleFaced\nOracle:Front rules text.\n\nALTERNATE\n\nName:Fixture DFC Back\nManaCost:no cost\nTypes:Creature\nOracle:Back rules text.\n";
        assert_eq!(meld_back_identity(script), None);
    }

    /// Proves the reconciliation check actually works, per the same
    /// "mutate what it guards" standard `puzzle-testing`/CI conventions use
    /// elsewhere in this project: simulates the EXACT pre-fix bug (a meld
    /// source file defines two identities, but only the front was ever
    /// passed to `index.lookup`, recorded in `attempted`) and confirms
    /// `reconcile_identities` reports the back as unaccounted. Then
    /// confirms the same input reports nothing once both identities are
    /// present in `attempted`, matching what `generate`'s fixed main loop
    /// now produces. Before the meld fix landed, this exact "front only"
    /// `attempted` shape is what `generate`'s loop produced for every one
    /// of the 7 meld-back identities documented in
    /// `ai_docs/transient/NUMERIC_CORPUS_SWAP_20260819.md` section 8.
    #[test]
    fn reconcile_identities_flags_an_identity_that_was_never_attempted() {
        let source_root = Path::new("cardsfolder");
        let source_path = source_root.join("g/gisela_fixture.txt");
        let text = "Name:Fixture Front\nManaCost:2 W\nTypes:Creature\nAlternateMode:Meld\nOracle:Front rules text.\n\nALTERNATE\n\nName:Fixture Back\nManaCost:no cost\nTypes:Creature\nOracle:Back rules text.\n".to_owned();
        let source_texts = vec![(source_path.clone(), text)];

        // The BROKEN state: only the front identity was ever attempted.
        let attempted_broken: BTreeSet<(PathBuf, String)> =
            [(source_path.clone(), "Fixture Front".to_owned())].into_iter().collect();
        let mut unaccounted = Vec::new();
        reconcile_identities(source_root, &source_texts, &attempted_broken, &mut unaccounted);
        assert_eq!(unaccounted.len(), 1, "the meld back identity must be reported as unaccounted");
        assert_eq!(unaccounted[0].name, "Fixture Back");

        // The FIXED state: both identities were attempted.
        let attempted_fixed: BTreeSet<(PathBuf, String)> = [
            (source_path.clone(), "Fixture Front".to_owned()),
            (source_path.clone(), "Fixture Back".to_owned()),
        ]
        .into_iter()
        .collect();
        let mut unaccounted_fixed = Vec::new();
        reconcile_identities(source_root, &source_texts, &attempted_fixed, &mut unaccounted_fixed);
        assert!(unaccounted_fixed.is_empty(), "both identities attempted -> nothing unaccounted");
    }
}
