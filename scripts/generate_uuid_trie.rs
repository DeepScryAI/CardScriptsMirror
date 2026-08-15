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

    /// Generated three-level numeric-ID trie.
    #[arg(long, default_value = "cards")]
    output: PathBuf,

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

#[derive(Default)]
struct CatalogIndex {
    by_oracle_id: BTreeMap<OracleId, Vec<CardScriptId>>,
    by_name_hash: BTreeMap<String, Option<CardScriptId>>,
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

    let report = generate(&args.source, &args.output, &index, &catalog)?;
    write_report(&report)?;
    print_report(&report);

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
    let color_identity = ["W", "U", "B", "R", "G"]
        .into_iter()
        .filter(|color| card.color_identity.iter().any(|present| present == color))
        .collect::<String>();
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

fn generate(source: &Path, output: &Path, index: &NameIndex, catalog: &CatalogIndex) -> Result<GenerationReport> {
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
        let oracle_ids = match index.lookup(&name) {
            Some(candidates) => match resolve_oracle_ids(&source_text, candidates) {
                Some(ids) => ids,
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
        for oracle_id in oracle_ids {
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
                let sanitized = sanitize_script(&source_text, card_id, color_identity, &numeric_name_refs);
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

fn replace_named_qualifiers(line: &str, references: &[(String, CardScriptId)]) -> String {
    let mut output = String::with_capacity(line.len());
    let mut remaining = line;
    while let Some(offset) = remaining.find("named") {
        output.push_str(&remaining[..offset]);
        let after_marker = &remaining[offset + "named".len()..];
        let replacement = references.iter().find(|(name, _)| {
            after_marker.strip_prefix(name).is_some_and(|tail| {
                tail.chars().next().is_none_or(|next| {
                    next.is_whitespace() || matches!(next, '.' | '+' | ',' | '/' | '$' | '>' | ')' | ';')
                })
            })
        });
        if let Some((name, card_id)) = replacement {
            output.push_str(&format!("catalogId{}", card_id.0));
            remaining = &after_marker[name.len()..];
        } else {
            output.push_str("named");
            remaining = after_marker;
        }
    }
    output.push_str(remaining);
    output
}

fn numeric_id_for_name(value: &str, references: &[(String, CardScriptId)]) -> Option<CardScriptId> {
    let normalized = value.trim().replace(';', ",");
    references
        .iter()
        .find_map(|(name, id)| (name == &normalized).then_some(*id))
}

fn rewrite_top_level_card_reference(line: &str, references: &[(String, CardScriptId)]) -> Option<String> {
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

fn sanitize_script(
    script: &str,
    card_id: CardScriptId,
    color_identity: &str,
    numeric_name_refs: &[(String, CardScriptId)],
) -> String {
    let mut output = String::with_capacity(script.len());
    output.push_str(&format!("Id:{}\n", card_id.0));
    output.push_str(&format!("ColorIdentity:{color_identity}\n"));
    for line in script.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') || is_removed_top_level_field(line) {
            continue;
        }
        let numeric = replace_named_qualifiers(line, numeric_name_refs);
        let numeric = rewrite_card_reference_parameters(&numeric, numeric_name_refs);
        let sanitized = strip_display_parameters(&numeric);
        output.push_str(&sanitized);
        output.push('\n');
    }
    output
}

fn is_removed_top_level_field(line: &str) -> bool {
    let mut fields = line.split(':').map(str::trim);
    match fields.next() {
        // These fields only guide Forge's deck builder, draft picker, or AI
        // deck selection and are not consumed by the gameplay DSL. They also
        // frequently contain card titles, so the anonymous runtime corpus
        // must not retain them.
        Some("Name" | "Oracle" | "Text" | "DeckHints" | "ODeckHints" | "DeckHas" | "DeckNeeds" | "Draft" | "AI") => {
            true
        }
        Some("Variant") => {
            let _variant_id = fields.next();
            matches!(fields.next(), Some("Name" | "Oracle"))
        }
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
            key.ends_with("Description") || key.ends_with("Prompt") || key.ends_with("Desc") || key == "ChoiceTitle"
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
            sanitize_script(input, CardScriptId(145), "R", &[]),
            "Id:145\nColorIdentity:R\nManaCost:R\nTypes:Instant\nA:SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3\nSVar:Named:DB$ MakeCard | Name$ Fixture Qzx One\n"
        );
    }

    #[test]
    fn removes_all_human_description_parameters() {
        let input = "Text:Display-only sentence.\nT:Mode$ SpellCast | TriggerDescription$ Display sentence | CostDesc$ More display text | Description$ Keep this\n";
        assert_eq!(
            sanitize_script(input, CardScriptId(1), "", &[]),
            "Id:1\nColorIdentity:\nT:Mode$ SpellCast\n"
        );
    }

    #[test]
    fn removes_non_runtime_deck_hints() {
        let input = "# Human implementation note\n\nDeckHints:Type$Forest & Name$Fixture Qzx One\nDeckHas:Ability$Token\nDeckNeeds:Name$Fixture Qzx Two\nDraft:AI$ True\nAI:RemoveDeck:Random\nManaCost:G\nTypes:Creature\n";
        assert_eq!(
            sanitize_script(input, CardScriptId(1), "G", &[]),
            "Id:1\nColorIdentity:G\nManaCost:G\nTypes:Creature\n"
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
            card_faces: vec![
                ScryfallFace {
                    name: "Fire".to_owned(),
                    printed_name: None,
                    oracle_text: None,
                    mana_cost: "{1}{R}".to_owned(),
                    type_line: "Instant".to_owned(),
                },
                ScryfallFace {
                    name: "Ice".to_owned(),
                    printed_name: None,
                    oracle_text: None,
                    mana_cost: "{1}{U}".to_owned(),
                    type_line: "Instant".to_owned(),
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
}
