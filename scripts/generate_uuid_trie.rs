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
//! unicode-normalization = "0.1.24"
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
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};
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

    /// Explicit, owner-approved source names to exclude from the generated
    /// corpus. Any unmapped source not listed here remains fatal.
    #[arg(long)]
    exclude_file: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct GenerationReport {
    source_scripts: usize,
    generated_scripts: usize,
    duplicate_identical_scripts: usize,
    excluded_scripts: Vec<MappingProblem>,
    missing_mappings: Vec<MappingProblem>,
    ambiguous_mappings: Vec<MappingProblem>,
    conflicting_scripts: Vec<ScriptConflict>,
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
    normalized_whole_cards: BTreeMap<String, BTreeMap<OracleId, IdentityEvidence>>,
    normalized_faces: BTreeMap<String, BTreeMap<OracleId, IdentityEvidence>>,
    color_identities: BTreeMap<OracleId, String>,
}

#[derive(Clone, Debug, Default)]
struct IdentityEvidence {
    oracle_texts: BTreeSet<String>,
    raw_oracle_texts: BTreeSet<String>,
    signatures: BTreeSet<CardSignature>,
    collector_numbers: BTreeSet<String>,
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
        collector_number: Option<&str>,
    ) {
        insert_identity(
            &mut self.whole_cards,
            name,
            oracle_id.clone(),
            oracle_text,
            signature.clone(),
            collector_number,
        );
        insert_identity(
            &mut self.normalized_whole_cards,
            &normalize_card_name(name),
            oracle_id,
            oracle_text,
            signature,
            collector_number,
        );
    }

    fn insert_face(
        &mut self,
        name: &str,
        oracle_id: OracleId,
        oracle_text: Option<&str>,
        signature: CardSignature,
        collector_number: Option<&str>,
    ) {
        insert_identity(
            &mut self.faces,
            name,
            oracle_id.clone(),
            oracle_text,
            signature.clone(),
            collector_number,
        );
        insert_identity(
            &mut self.normalized_faces,
            &normalize_card_name(name),
            oracle_id,
            oracle_text,
            signature,
            collector_number,
        );
    }

    fn lookup(&self, name: &str) -> Option<&BTreeMap<OracleId, IdentityEvidence>> {
        // A complete card name is authoritative. Face aliases are needed for
        // Forge's split/adventure face scripts, but a new multi-face card can
        // legitimately reuse the name of an older standalone card.
        self.whole_cards
            .get(name.trim())
            .or_else(|| self.faces.get(name.trim()))
            .or_else(|| {
                let normalized = normalize_card_name(name);
                self.normalized_whole_cards
                    .get(&normalized)
                    .or_else(|| self.normalized_faces.get(&normalized))
            })
    }

    fn distinct_name_count(&self) -> usize {
        self.whole_cards
            .keys()
            .chain(self.faces.keys())
            .collect::<BTreeSet<_>>()
            .len()
    }
}

/// Conservative join key for Forge/Scryfall spelling differences. Unicode
/// compatibility decomposition removes accents; punctuation and spaces are
/// ignored. Collisions remain candidate sets and are never guessed away.
fn normalize_card_name(name: &str) -> String {
    name.nfkd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn replace_chars(value: &str, source: char, replacement: char) -> String {
    value
        .chars()
        .map(|character| (character == source).then_some(replacement).unwrap_or(character))
        .collect()
}

/// Replace a controlled vocabulary token without applying a free-form string
/// rewrite to the card-script DSL. Callers use this only after selecting the
/// structured field/API whose vocabulary is being normalized.
fn replace_literal(value: &str, source: &str, replacement: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut remainder = value;
    while let Some((prefix, suffix)) = remainder.split_once(source) {
        output.push_str(prefix);
        output.push_str(replacement);
        remainder = suffix;
    }
    output.push_str(remainder);
    output
}

fn insert_identity(
    index: &mut BTreeMap<String, BTreeMap<OracleId, IdentityEvidence>>,
    name: &str,
    oracle_id: OracleId,
    oracle_text: Option<&str>,
    signature: CardSignature,
    collector_number: Option<&str>,
) {
    let evidence = index
        .entry(name.trim().to_owned())
        .or_default()
        .entry(oracle_id)
        .or_default();
    if let Some(oracle_text) = oracle_text {
        evidence.oracle_texts.insert(normalize_oracle_text(oracle_text));
        evidence.raw_oracle_texts.insert(oracle_text.to_owned());
    }
    evidence.signatures.insert(signature);
    if let Some(collector_number) = collector_number {
        evidence.collector_numbers.insert(collector_number.to_owned());
    }
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

    let exclusions = load_exclusions(args.exclude_file.as_deref())?;

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
    let report = generate(&args.source, &args.output, &index, &catalog, &token_index, &exclusions)?;
    generate_tokens(&token_source, &args.token_output, &index, &catalog, &token_index)?;
    write_report(&report)?;
    print_report(&report);

    let mapping_problem_count = report.missing_mappings.len() + report.ambiguous_mappings.len();
    if !report.conflicting_scripts.is_empty() {
        bail!(
            "generation found {} UUID collisions with different sanitized scripts; see .cache/reports/generate-report.json",
            report.conflicting_scripts.len()
        );
    }
    if mapping_problem_count > 0 {
        bail!(
            "generation rejected {mapping_problem_count} missing or ambiguous Scryfall mappings; see .cache/reports/generate-report.json"
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

fn load_exclusions(path: Option<&Path>) -> Result<BTreeSet<String>> {
    let Some(path) = path else {
        return Ok(BTreeSet::new());
    };
    let content = fs::read_to_string(path).with_context(|| format!("read exclusion file {}", path.display()))?;
    let mut names = BTreeSet::new();
    for (line_number, line) in content.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() < 4 {
            bail!(
                "exclusion file {} line {} has fewer than four tab-separated fields",
                path.display(),
                line_number + 1
            );
        }
        let name = fields[0].trim();
        let status = fields[2].trim();
        if name.is_empty() {
            bail!(
                "exclusion file {} line {} has an empty name",
                path.display(),
                line_number + 1
            );
        }
        if status != "OWNER_APPROVED_SKIP" {
            eprintln!(
                "NOTE: exclusion {} line {} is not active (status={status:?}); it cannot suppress a generation failure",
                path.display(),
                line_number + 1
            );
            continue;
        }
        if !names.insert(name.to_owned()) {
            bail!("exclusion file {} repeats {name:?}", path.display());
        }
    }
    Ok(names)
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
    if matches!(
        card.layout.as_str(),
        "art_series" | "double_faced_token" | "emblem" | "front_card" | "token"
    ) {
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
        Some(&card.collector_number),
    );
    if let Some(printed_name) = card.printed_name {
        index.insert_whole_card(
            &printed_name,
            oracle_id.clone(),
            card.oracle_text.as_deref(),
            whole_signature,
            Some(&card.collector_number),
        );
    }
    for face in card.card_faces {
        let face_signature = CardSignature(vec![scryfall_face_signature(&face)]);
        index.insert_face(
            &face.name,
            oracle_id.clone(),
            face.oracle_text.as_deref(),
            face_signature.clone(),
            Some(&card.collector_number),
        );
        if let Some(printed_name) = face.printed_name {
            index.insert_face(
                &printed_name,
                oracle_id.clone(),
                face.oracle_text.as_deref(),
                face_signature,
                Some(&card.collector_number),
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
    exclusions: &BTreeSet<String>,
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
        excluded_scripts: Vec::new(),
        missing_mappings: Vec::new(),
        ambiguous_mappings: Vec::new(),
        conflicting_scripts: Vec::new(),
    };
    let mut generated: BTreeMap<CardScriptId, (PathBuf, String)> = BTreeMap::new();
    let numeric_name_refs = numeric_name_references(index, catalog);

    for source_path in sources {
        let source_text =
            fs::read_to_string(&source_path).with_context(|| format!("read Forge script {}", source_path.display()))?;
        let name = match source_identity_name(&source_text) {
            Some(name) => name,
            None => {
                report.missing_mappings.push(MappingProblem {
                    source: relative_display(source, &source_path),
                    name: "<missing Name field>".to_owned(),
                    oracle_ids: Vec::new(),
                });
                continue;
            }
        };
        if exclusions.contains(&name) {
            report.excluded_scripts.push(MappingProblem {
                source: relative_display(source, &source_path),
                name,
                oracle_ids: Vec::new(),
            });
            continue;
        }
        let unsupported_variant = top_level_value(&source_text, "Oracle") == Some("<Unsupported Variant>");
        let resolved_scripts = match index.lookup(&name) {
            Some(candidates) => match if unsupported_variant {
                resolve_unsupported_variant_records(&source_text, &name, candidates)
            } else {
                resolve_oracle_ids(&source_text, candidates).map(|ids| {
                    ids.into_iter()
                        .map(|oracle_id| (oracle_id, source_text.clone()))
                        .collect()
                })
            } {
                Some(records) => records,
                None => {
                    report.ambiguous_mappings.push(MappingProblem {
                        source: relative_display(source, &source_path),
                        name: name.clone(),
                        oracle_ids: candidates.keys().map(|id| id.0.hyphenated().to_string()).collect(),
                    });
                    continue;
                }
            },
            None => {
                report.missing_mappings.push(MappingProblem {
                    source: relative_display(source, &source_path),
                    name: name.clone(),
                    oracle_ids: Vec::new(),
                });
                continue;
            }
        };
        // A Forge title may have several Scryfall Oracle identities (notably
        // silver-border variants).  The anonymous catalog is the authority
        // for which identities are representable: accept only when exactly
        // one candidate has a catalog row.  Never pick an arbitrary candidate.
        let catalog_oracle_ids: Vec<_> = resolved_scripts
            .iter()
            .filter(|(oracle_id, _)| catalog.by_oracle_id.contains_key(oracle_id))
            .map(|(oracle_id, script)| (oracle_id.clone(), script.clone()))
            .collect();
        let resolved_scripts = if !unsupported_variant && catalog_oracle_ids.len() == 1 {
            catalog_oracle_ids
        } else {
            resolved_scripts
        };
        for (oracle_id, selected_script) in resolved_scripts {
            let Some(card_ids) = catalog.by_oracle_id.get(&oracle_id) else {
                report.missing_mappings.push(MappingProblem {
                    source: relative_display(source, &source_path),
                    name: name.clone(),
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
                let sanitized = sanitize_script(
                    &selected_script,
                    card_id,
                    color_identity,
                    set_group,
                    &numeric_name_refs,
                    token_index,
                );
                if let Some((first_path, first_text)) = generated.get(&card_id) {
                    if first_text == &sanitized {
                        report.duplicate_identical_scripts += 1;
                    } else {
                        report.conflicting_scripts.push(ScriptConflict {
                            card_id: card_id.0,
                            first_source: relative_display(source, first_path),
                            second_source: relative_display(source, &source_path),
                        });
                    }
                    continue;
                }

                let destination = card_id.trie_path(&stage);
                let parent = destination.parent().context("generated path has no parent")?;
                fs::create_dir_all(parent).with_context(|| format!("create trie directory {}", parent.display()))?;
                fs::write(&destination, sanitized.as_bytes())
                    .with_context(|| format!("write generated script {}", destination.display()))?;
                generated.insert(card_id, (source_path.clone(), sanitized));
                report.generated_scripts += 1;
            }
        }
    }

    publish_directory(&stage, output)?;
    Ok(report)
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct VariantRecord {
    label: String,
    oracle_text: String,
}

fn unsupported_variant_records(script: &str) -> Vec<VariantRecord> {
    script
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Variant:")?;
            let (label, rest) = rest.split_once(':')?;
            let (field, value) = rest.split_once(':')?;
            (field == "Oracle").then(|| VariantRecord {
                label: label.to_owned(),
                oracle_text: value.to_owned(),
            })
        })
        .collect()
}

fn materialize_variant(script: &str, label: &str) -> String {
    let prefix = format!("Variant:{label}:");
    script
        .lines()
        .filter_map(|line| {
            if let Some(materialized) = line.strip_prefix(&prefix) {
                Some(materialized.to_owned())
            } else if line.starts_with("Variant:") {
                None
            } else {
                Some(line.to_owned())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn oracle_comparison_tokens(text: &str, own_name: &str) -> Vec<String> {
    let decoded = replace_literal(text, "\\n", "\n");
    let mut tokens = Vec::new();
    let mut word = String::new();
    for character in decoded.chars() {
        if character.is_alphanumeric() {
            word.extend(character.to_lowercase());
        } else {
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
            if !character.is_whitespace() {
                tokens.push(character.to_string());
            }
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }

    let own_tokens = tokenize_words(own_name);
    replace_token_sequence(&mut tokens, &own_tokens, "__self__");
    replace_token_sequence(&mut tokens, &tokenize_words("CARDNAME"), "__self__");
    for phrase in [
        "this creature",
        "this artifact",
        "this enchantment",
        "this land",
        "this permanent",
        "this planeswalker",
        "this spell",
        "this card",
    ] {
        replace_token_sequence(&mut tokens, &tokenize_words(phrase), "__self__");
    }
    tokens
}

fn tokenize_words(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|part| {
            part.chars()
                .filter(|character| character.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect()
        })
        .collect()
}

fn replace_token_sequence(tokens: &mut Vec<String>, pattern: &[String], replacement: &str) {
    if pattern.is_empty() {
        return;
    }
    let mut cursor = 0;
    while cursor + pattern.len() <= tokens.len() {
        if tokens[cursor..cursor + pattern.len()] == *pattern {
            tokens.splice(cursor..cursor + pattern.len(), [replacement.to_owned()]);
            cursor += 1;
        } else {
            cursor += 1;
        }
    }
}

fn expected_unstable_collector_suffix(name: &str, label: &str) -> Option<&'static str> {
    match (name, label) {
        ("Everythingamajig", "C") => Some("147c"),
        ("Garbage Elemental", "B") => Some("82b"),
        ("Garbage Elemental", "C") => Some("82c"),
        ("Garbage Elemental", "D") => Some("82d"),
        ("Sly Spy", "F") => Some("67f"),
        _ => None,
    }
}

fn resolve_unsupported_variant_records(
    script: &str,
    name: &str,
    candidates: &BTreeMap<OracleId, IdentityEvidence>,
) -> Option<Vec<(OracleId, String)>> {
    let variants = unsupported_variant_records(script);
    if variants.is_empty() {
        return None;
    }
    let mut resolved = Vec::with_capacity(variants.len());
    for variant in variants {
        let source_tokens = oracle_comparison_tokens(&variant.oracle_text, name);
        let matching: Vec<_> = candidates
            .iter()
            .filter(|(_, evidence)| {
                evidence
                    .raw_oracle_texts
                    .iter()
                    .any(|text| oracle_comparison_tokens(text, name) == source_tokens)
            })
            .map(|(oracle_id, _)| oracle_id.clone())
            .collect();
        if matching.len() != 1 {
            return None;
        }
        let oracle_id = matching.into_iter().next().expect("one variant match");
        if let Some(expected_suffix) = expected_unstable_collector_suffix(name, &variant.label) {
            let suffix_agrees = candidates
                .get(&oracle_id)
                .is_some_and(|evidence| evidence.collector_numbers.contains(expected_suffix));
            if !suffix_agrees {
                return None;
            }
        }
        if resolved.iter().any(|(existing, _)| existing == &oracle_id) {
            return None;
        }
        resolved.push((oracle_id, materialize_variant(script, &variant.label)));
    }
    Some(resolved)
}

fn resolve_oracle_ids(script: &str, candidates: &BTreeMap<OracleId, IdentityEvidence>) -> Option<Vec<OracleId>> {
    if candidates.len() == 1 {
        return Some(candidates.keys().cloned().collect());
    }
    if top_level_value(script, "Oracle") == Some("<Unsupported Variant>") {
        let name = source_identity_name(script)?;
        return resolve_unsupported_variant_records(script, &name, candidates)
            .map(|records| records.into_iter().map(|(oracle_id, _)| oracle_id).collect());
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
    replace_literal(text, "\\n", "\n")
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
        for alias in [
            name.clone(),
            replace_chars(name, ',', ';'),
            replace_chars(name, ' ', '_'),
        ] {
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

fn replace_named_qualifiers(line: &str, references: &[(String, CardScriptId)]) -> String {
    let mut output = Vec::new();
    for segment in line.split('|') {
        let trimmed = segment.trim();
        let rewritten = trimmed
            .split_once('$')
            .map(|(key, value_raw)| {
                let prefix = value_raw
                    .chars()
                    .next()
                    .filter(|character| character.is_whitespace())
                    .map_or("", |_| " ");
                format!(
                    "{}${prefix}{}",
                    key.trim(),
                    rewrite_named_value(value_raw.trim(), references)
                )
            })
            .unwrap_or_else(|| rewrite_named_value(trimmed, references));
        output.push(rewritten);
    }
    output.join(" | ")
}

fn rewrite_named_value(value: &str, references: &[(String, CardScriptId)]) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let remainder = &value[cursor..];
        let candidate = [
            ("attacking creatures named ", "attacking creatures catalogId"),
            ("creatures named ", "creatures catalogId"),
            ("creature named ", "creature catalogId"),
            ("lands named ", "lands catalogId"),
            ("land named ", "land catalogId"),
            ("!named", "!catalogId"),
            (".named", ".catalogId"),
        ]
        .into_iter()
        .find(|(prefix, _)| remainder.starts_with(prefix));

        let direct_named = remainder.starts_with("named")
            && (cursor == 0
                || value[..cursor]
                    .chars()
                    .next_back()
                    .is_none_or(|previous| !previous.is_alphanumeric() && previous != '_'));
        let Some((prefix, replacement)) = candidate.or_else(|| direct_named.then_some(("named", "namedcatalogId")))
        else {
            if let Some(character) = remainder.chars().next() {
                output.push(character);
                cursor += character.len_utf8();
            }
            continue;
        };

        let title_start = cursor + prefix.len();
        let title_remainder = &value[title_start..];
        let Some((name, id)) = references.iter().find(|(name, _)| {
            title_remainder.strip_prefix(name).is_some_and(|tail| {
                tail.chars().next().is_none_or(|next| {
                    next.is_whitespace() || matches!(next, '.' | ',' | '$' | ')' | ';' | '_' | '>' | '/' | '+' | ':')
                })
            })
        }) else {
            if let Some(character) = remainder.chars().next() {
                output.push(character);
                cursor += character.len_utf8();
            }
            continue;
        };

        output.push_str(replacement);
        output.push_str(&id.0.to_string());
        cursor = title_start + name.len();
    }
    output
}

fn numeric_id_for_name(value: &str, references: &[(String, CardScriptId)]) -> Option<CardScriptId> {
    let normalized = replace_chars(value.trim(), ';', ',');
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

    let segments = line.split('|');
    let api = line
        .split('|')
        .next()
        .and_then(|head| head.rsplit_once('$'))
        .map(|(_, value)| value.trim())
        .unwrap_or("");
    let mut output = Vec::new();
    for segment in segments {
        let trimmed = segment.trim();
        let Some((key, value_raw)) = trimmed.split_once('$') else {
            output.push(trimmed.to_owned());
            continue;
        };
        let key = key.trim();
        let value = value_raw.trim();
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
    let api = line
        .split('|')
        .next()
        .and_then(|head| head.rsplit_once('$'))
        .map(|(_, value)| value.trim())
        .unwrap_or("");
    let mut output = Vec::new();
    let segments = line.split('|');
    for segment in segments {
        let trimmed = segment.trim();
        let Some((key, value_raw)) = trimmed.split_once('$') else {
            output.push(trimmed.to_owned());
            continue;
        };
        let key = key.trim();
        let value = value_raw.trim();
        let value_prefix = value_raw
            .chars()
            .next()
            .filter(|character| character.is_whitespace())
            .map_or("", |_| " ");
        let is_object_name = matches!(key, "NewName" | "SetName")
            || (key == "Name" && matches!(api, "Effect" | "ReplaceEffect" | "ReplaceSplitDamage" | "Animate"));
        if is_object_name {
            output.push(format!("{key}${value_prefix}{}", stable_runtime_object_id(value)));
        } else {
            let rewritten = value
                .split('+')
                .map(|body| {
                    body.split('_')
                        .map(|part| {
                            runtime_ids
                                .iter()
                                .find_map(|(name, id)| {
                                    [
                                        format!("named{name}"),
                                        format!("named{}", replace_chars(name, ',', ';')),
                                        format!("Effect.named{name}"),
                                        format!("Effect.named{}", replace_chars(name, ',', ';')),
                                    ]
                                    .into_iter()
                                    .find_map(|prefix| {
                                        part.strip_prefix(&prefix).map(|tail| {
                                            let marker = if prefix.starts_with("Effect.") {
                                                "Effect.named"
                                            } else {
                                                "named"
                                            };
                                            format!("{marker}{id}{tail}")
                                        })
                                    })
                                })
                                .unwrap_or_else(|| part.to_owned())
                        })
                        .collect::<Vec<_>>()
                        .join("_")
                })
                .collect::<Vec<_>>()
                .join("+");
            output.push(format!("{key}${value_prefix}{rewritten}"));
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
        replace_literal(&text, source, replacement)
    });

    if normalized.starts_with("K:Spend only colored mana on X.") {
        return "K:DistinctColoredManaForX".to_owned();
    }

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
    let mut output = replace_literal(line, "/Swamp card>", ">");
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
    let numeric = replace_named_qualifiers(line, numeric_name_refs);
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
        "Unmapped: {}; ambiguous: {}; conflicting numeric scripts: {}",
        report.missing_mappings.len(),
        report.ambiguous_mappings.len(),
        report.conflicting_scripts.len()
    );
    eprintln!(
        "Explicitly excluded by owner-approved list: {}",
        report.excluded_scripts.len()
    );
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
                &references
            ),
            "S:Mode$ Continuous | Affected$ Creature.catalogId145+YouCtrl"
        );
    }

    #[test]
    fn named_rewrite_covers_nested_negated_and_description_qualifiers() {
        let refs = vec![("Fixture Dragon".to_owned(), CardScriptId(24491))];
        assert_eq!(
            replace_named_qualifiers(
                "SVar:X:Count$Valid Creature.Dragon+!namedFixture Dragon+YouCtrl$CardManaCost",
                &refs,
            ),
            "SVar:X:Count$Valid Creature.Dragon+!catalogId24491+YouCtrl$CardManaCost"
        );
        assert_eq!(
            replace_named_qualifiers(
                "A:AB$ ChangeZone | Cost$ tapXType<5/Creature.attacking+namedFixture Dragon/attacking creatures named Fixture Dragon>",
                &refs,
            ),
            "A:AB$ ChangeZone | Cost$ tapXType<5/Creature.attacking+namedcatalogId24491/attacking creatures catalogId24491>"
        );
        assert_eq!(
            replace_named_qualifiers("SVar:X:Count$Valid Creature.namedFixture Dragon+YouCtrl", &refs,),
            "SVar:X:Count$Valid Creature.catalogId24491+YouCtrl"
        );
        assert_eq!(
            replace_named_qualifiers(
                "K:Bands with other:Creature.namedFixture Dragon:creatures named Fixture Dragon",
                &refs,
            ),
            "K:Bands with other:Creature.catalogId24491:creatures catalogId24491"
        );
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
            collector_number: String::new(),
            mana_cost: String::new(),
            type_line: "Land — Mountain Plains".to_owned(),
            lang: "en".to_owned(),
            layout: "normal".to_owned(),
            color_identity: vec!["R".to_owned(), "W".to_owned()],
            colors: Vec::new(),
            color_indicator: None,
            card_faces: Vec::new(),
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
            collector_number: String::new(),
            mana_cost: String::new(),
            type_line: "Land — Mountain Plains".to_owned(),
            lang: "en".to_owned(),
            layout: "normal".to_owned(),
            color_identity: vec!["R".to_owned(), "W".to_owned()],
            colors: Vec::new(),
            color_indicator: None,
            card_faces: Vec::new(),
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
            collector_number: String::new(),
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
                },
                ScryfallFace {
                    name: "Ice".to_owned(),
                    printed_name: None,
                    oracle_text: None,
                    mana_cost: "{1}{U}".to_owned(),
                    type_line: "Instant".to_owned(),
                    colors: vec!["U".to_owned()],
                    color_indicator: None,
                },
            ],
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

    fn variant_candidate(oracle_text: &str, collector_number: &str) -> IdentityEvidence {
        IdentityEvidence {
            raw_oracle_texts: BTreeSet::from([oracle_text.to_owned()]),
            collector_numbers: BTreeSet::from([collector_number.to_owned()]),
            ..IdentityEvidence::default()
        }
    }

    #[test]
    fn unsupported_variant_refuses_zero_and_multiple_mechanics_matches() {
        let script = "Name:Fixture Variant\nOracle:<Unsupported Variant>\nVariant:C:Oracle:Fixture Variant does something.\nVariant:C:A:AB$ Test\n";
        let empty = BTreeMap::new();
        assert!(resolve_unsupported_variant_records(script, "Fixture Variant", &empty).is_none());

        let first = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let second = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let candidates = BTreeMap::from([
            (
                OracleId(first),
                variant_candidate("Fixture Variant does something.", "1"),
            ),
            (
                OracleId(second),
                variant_candidate("Fixture Variant does something.", "2"),
            ),
        ]);
        assert!(resolve_unsupported_variant_records(script, "Fixture Variant", &candidates).is_none());
    }

    #[test]
    fn unsupported_variant_materializes_mechanics_before_sanitizing() {
        let oracle_id = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
        let candidates = BTreeMap::from([(
            OracleId(oracle_id),
            variant_candidate("This artifact becomes an X/X creature.", "147c"),
        )]);
        let script = "Name:Everythingamajig\nManaCost:5\nTypes:Artifact\nOracle:<Unsupported Variant>\nVariant:C:A:AB$ Animate | Power$ X | Toughness$ X\nVariant:C:Oracle:Everythingamajig becomes an X/X creature.\n";
        let records = resolve_unsupported_variant_records(script, "Everythingamajig", &candidates).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].1.contains("A:AB$ Animate"));
        assert!(!records[0].1.contains("Variant:C:"));
        let sanitized = sanitize_script(&records[0].1, CardScriptId(1), "", "Gfixture", &[], &BTreeMap::new());
        assert!(!sanitized.contains("Oracle:<Unsupported Variant>"));
        assert!(sanitized.contains("A:AB$ Animate"));
    }

    #[test]
    fn unsupported_variant_refuses_collector_suffix_disagreement() {
        let oracle_id = Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();
        let candidates = BTreeMap::from([(
            OracleId(oracle_id),
            variant_candidate("This artifact becomes an X/X creature.", "147d"),
        )]);
        let script = "Name:Everythingamajig\nOracle:<Unsupported Variant>\nVariant:C:Oracle:Everythingamajig becomes an X/X creature.\n";
        assert!(resolve_unsupported_variant_records(script, "Everythingamajig", &candidates).is_none());
    }
}
