#!/usr/bin/env rust-script
//! ```cargo
//! [package]
//! edition = "2021"
//!
//! [dependencies]
//! anyhow = "1.0.99"
//! clap = { version = "4.5.45", features = ["derive"] }
//! flate2 = "1.1.2"
//! reqwest = { version = "0.12.23", features = ["blocking", "json", "rustls-tls"], default-features = false }
//! serde = { version = "1.0.219", features = ["derive"] }
//! serde_json = "1.0.143"
//! sha2 = "0.10.9"
//! uuid = { version = "1.18.0", features = ["serde"] }
//! ```

#[path = "lib/scryfall_bulk.rs"]
mod scryfall_bulk;

use anyhow::{bail, Context, Result};
use clap::Parser;
use scryfall_bulk::{ensure_cache, for_each_card};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const SET_REGISTRY_HEADER: &str = "#anonymous_set_id\tscryfall_set_id";
const CATALOG_HEADER: &str = "#id\toracle_id\tname_sha256\tanonymous_set_id";

#[derive(Parser, Debug)]
#[command(about = "Extract the anonymous numeric/Oracle identity bridge from DeepScry's catalog")]
struct Args {
    #[arg(long)]
    source: PathBuf,
    #[arg(long, default_value = "catalog_ids.tsv")]
    output: PathBuf,
    #[arg(long, default_value = "set_ids.tsv")]
    set_registry: PathBuf,
    #[arg(long, default_value = ".cache/scryfall/default_cards.json")]
    cache: PathBuf,
    #[arg(long)]
    refresh: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogRow {
    id: u32,
    oracle_id: Uuid,
    name_hash: String,
    publisher_set_code: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SetMetadata {
    set_id: Uuid,
    publisher_code: String,
    released_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SetAssignment {
    anonymous_id: String,
    set_id: Uuid,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let source =
        fs::read_to_string(&args.source).with_context(|| format!("read DeepScry catalog {}", args.source.display()))?;
    let rows = parse_catalog(&source)?;
    ensure_cache(&args.cache, args.refresh)?;
    let metadata = load_set_metadata(&args.cache)?;
    let assignments = assign_sets(&rows, &metadata, read_set_registry(&args.set_registry)?)?;
    publish(&args.set_registry, &render_set_registry(&assignments)?)?;
    publish(&args.output, &render_catalog(&rows, &metadata, &assignments)?)?;
    eprintln!(
        "Wrote {} card identities and {} frozen set identities",
        rows.len(),
        assignments.len()
    );
    Ok(())
}

fn publish(path: &Path, contents: &str) -> Result<()> {
    let temporary = path.with_extension("write-part");
    fs::write(&temporary, contents).with_context(|| format!("write temporary output {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("publish {}", path.display()))
}

fn parse_catalog(source: &str) -> Result<Vec<CatalogRow>> {
    let mut lines = source.lines();
    let header = lines.next().context("DeepScry catalog is empty")?;
    let columns: Vec<&str> = header.split('\t').collect();
    let id_column = column_index(&columns, "#id")?;
    let name_column = column_index(&columns, "name")?;
    let first_set_column = column_index(&columns, "first_set")?;
    let oracle_id_column = column_index(&columns, "oracle_id")?;
    let mut rows = Vec::new();
    let mut ids = BTreeSet::new();
    let mut identities = HashSet::new();
    for (offset, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_number = offset + 2;
        let fields: Vec<&str> = line.split('\t').collect();
        let id: u32 = field(&fields, id_column, line_number, "id")?
            .parse()
            .with_context(|| format!("invalid numeric id on line {line_number}"))?;
        if id == 0 || !ids.insert(id) {
            bail!("line {line_number} has zero or duplicate numeric id {id}");
        }
        let name = field(&fields, name_column, line_number, "name")?;
        if name.is_empty() {
            bail!("line {line_number} has an empty card name");
        }
        let oracle_id = field(&fields, oracle_id_column, line_number, "oracle_id")?
            .parse::<Uuid>()
            .with_context(|| format!("invalid oracle UUID on line {line_number}"))?;
        let name_hash = hex_sha256(name.as_bytes());
        if !identities.insert((oracle_id, name_hash.clone())) {
            bail!("line {line_number} duplicates an Oracle UUID/name identity");
        }
        rows.push(CatalogRow {
            id,
            oracle_id,
            name_hash,
            publisher_set_code: field(&fields, first_set_column, line_number, "first_set")?
                .trim()
                .to_ascii_lowercase(),
        });
    }
    if rows.is_empty() {
        bail!("DeepScry catalog contains no card rows");
    }
    Ok(rows)
}

fn load_set_metadata(cache: &Path) -> Result<HashMap<String, SetMetadata>> {
    let mut by_code: HashMap<String, SetMetadata> = HashMap::new();
    let mut conflict = None;
    for_each_card(cache, |card| {
        if conflict.is_some() {
            return;
        }
        let metadata = SetMetadata {
            set_id: card.set_id,
            publisher_code: card.set.to_ascii_lowercase(),
            released_at: card.released_at,
        };
        match by_code.get_mut(&metadata.publisher_code) {
            Some(existing) if existing.set_id != metadata.set_id => {
                conflict = Some(format!(
                    "Scryfall set code {:?} maps to multiple set UUIDs",
                    metadata.publisher_code
                ));
            }
            Some(existing) => {
                // Scryfall's `released_at` is attached to a printing. Rolling
                // sets such as The List legitimately contain several dates;
                // their stable set UUID is the identity and the earliest
                // printing date supplies the initial chronological order.
                if metadata.released_at < existing.released_at {
                    existing.released_at = metadata.released_at;
                }
            }
            None => {
                by_code.insert(metadata.publisher_code.clone(), metadata);
            }
        }
    })?;
    if let Some(message) = conflict {
        bail!(message);
    }
    Ok(by_code)
}

fn read_set_registry(path: &Path) -> Result<Vec<SetAssignment>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let source = fs::read_to_string(path).with_context(|| format!("read frozen set registry {}", path.display()))?;
    let mut lines = source.lines();
    if lines.next() != Some(SET_REGISTRY_HEADER) {
        bail!("unexpected frozen set registry header in {}", path.display());
    }
    let mut assignments = Vec::new();
    let mut anonymous_ids = HashSet::new();
    let mut set_ids = HashSet::new();
    for (offset, line) in lines.enumerate() {
        let line_number = offset + 2;
        let (anonymous_id, raw_set_id) = line
            .split_once('\t')
            .with_context(|| format!("set registry line {line_number} has no tab"))?;
        let set_id = raw_set_id
            .parse::<Uuid>()
            .with_context(|| format!("invalid Scryfall set UUID on registry line {line_number}"))?;
        validate_anonymous_set_id(anonymous_id)
            .with_context(|| format!("invalid anonymous set ID on registry line {line_number}"))?;
        if !anonymous_ids.insert(anonymous_id.to_string()) || !set_ids.insert(set_id) {
            bail!("set registry line {line_number} duplicates an ID or UUID");
        }
        assignments.push(SetAssignment {
            anonymous_id: anonymous_id.to_string(),
            set_id,
        });
    }
    Ok(assignments)
}

fn assign_sets(
    rows: &[CatalogRow],
    metadata: &HashMap<String, SetMetadata>,
    mut assignments: Vec<SetAssignment>,
) -> Result<Vec<SetAssignment>> {
    let needed_codes: BTreeSet<&str> = rows.iter().map(|row| row.publisher_set_code.as_str()).collect();
    let assigned_set_ids: HashSet<Uuid> = assignments.iter().map(|entry| entry.set_id).collect();
    let mut new_sets = Vec::new();
    for code in needed_codes {
        let set = metadata
            .get(code)
            .with_context(|| format!("Scryfall snapshot has no metadata for first-set code {code:?}"))?;
        if !assigned_set_ids.contains(&set.set_id) {
            new_sets.push(set.clone());
        }
    }
    new_sets.sort_by(|left, right| {
        left.released_at
            .cmp(&right.released_at)
            .then_with(|| left.publisher_code.cmp(&right.publisher_code))
    });
    let mut next_by_year: BTreeMap<u16, usize> = BTreeMap::new();
    for entry in &assignments {
        let (year, ordinal) = parse_anonymous_set_id(&entry.anonymous_id)?;
        next_by_year
            .entry(year)
            .and_modify(|next| *next = (*next).max(ordinal + 1))
            .or_insert(ordinal + 1);
    }
    for set in new_sets {
        let year = release_year(&set.released_at)?;
        let ordinal = next_by_year.entry(year).or_insert(0);
        assignments.push(SetAssignment {
            anonymous_id: format!("{year}{}", alpha_suffix(*ordinal)),
            set_id: set.set_id,
        });
        *ordinal += 1;
    }
    Ok(assignments)
}

fn render_catalog(
    rows: &[CatalogRow],
    metadata: &HashMap<String, SetMetadata>,
    assignments: &[SetAssignment],
) -> Result<String> {
    let by_set_id: HashMap<Uuid, &str> = assignments
        .iter()
        .map(|entry| (entry.set_id, entry.anonymous_id.as_str()))
        .collect();
    let mut output = format!("{CATALOG_HEADER}\n");
    for row in rows {
        let set = metadata
            .get(&row.publisher_set_code)
            .with_context(|| format!("missing set metadata for {:?}", row.publisher_set_code))?;
        let anonymous_id = by_set_id
            .get(&set.set_id)
            .with_context(|| format!("missing frozen assignment for Scryfall set {}", set.set_id))?;
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            row.id,
            row.oracle_id.hyphenated(),
            row.name_hash,
            anonymous_id
        ));
    }
    Ok(output)
}

fn render_set_registry(assignments: &[SetAssignment]) -> Result<String> {
    let mut output = format!("{SET_REGISTRY_HEADER}\n");
    for assignment in assignments {
        validate_anonymous_set_id(&assignment.anonymous_id)?;
        output.push_str(&format!(
            "{}\t{}\n",
            assignment.anonymous_id,
            assignment.set_id.hyphenated()
        ));
    }
    Ok(output)
}

fn release_year(released_at: &str) -> Result<u16> {
    released_at
        .get(..4)
        .context("release date has no four-digit year")?
        .parse()
        .with_context(|| format!("release date {released_at:?} has an invalid year"))
}

fn alpha_suffix(mut ordinal: usize) -> String {
    let mut reversed = Vec::new();
    loop {
        reversed.push((b'A' + (ordinal % 26) as u8) as char);
        if ordinal < 26 {
            break;
        }
        ordinal = ordinal / 26 - 1;
    }
    reversed.into_iter().rev().collect()
}

fn validate_anonymous_set_id(value: &str) -> Result<()> {
    parse_anonymous_set_id(value).map(|_| ())
}

fn parse_anonymous_set_id(value: &str) -> Result<(u16, usize)> {
    if value.len() < 5 {
        bail!("anonymous set ID {value:?} is shorter than YEAR+LETTER");
    }
    let year: u16 = value[..4]
        .parse()
        .with_context(|| format!("anonymous set ID {value:?} has no four-digit year"))?;
    let suffix = &value[4..];
    if !suffix.bytes().all(|byte| byte.is_ascii_uppercase()) {
        bail!("anonymous set ID {value:?} has a non-uppercase suffix");
    }
    let mut ordinal = 0usize;
    for byte in suffix.bytes() {
        ordinal = ordinal
            .checked_mul(26)
            .and_then(|value| value.checked_add(usize::from(byte - b'A') + 1))
            .context("anonymous set suffix is too large")?;
    }
    Ok((year, ordinal - 1))
}

fn column_index(columns: &[&str], wanted: &str) -> Result<usize> {
    columns
        .iter()
        .position(|column| *column == wanted)
        .with_context(|| format!("DeepScry catalog header has no {wanted:?} column"))
}

fn field<'a>(fields: &'a [&str], index: usize, line_number: usize, name: &str) -> Result<&'a str> {
    fields
        .get(index)
        .copied()
        .with_context(|| format!("catalog line {line_number} has no {name} field"))
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(last: u8) -> Uuid {
        Uuid::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, last])
    }

    fn row(id: u32, code: &str) -> CatalogRow {
        CatalogRow {
            id,
            oracle_id: uuid(id as u8),
            name_hash: "a".repeat(64),
            publisher_set_code: code.to_string(),
        }
    }

    fn set(last: u8, code: &str, date: &str) -> SetMetadata {
        SetMetadata {
            set_id: uuid(last),
            publisher_code: code.to_string(),
            released_at: date.to_string(),
        }
    }

    #[test]
    fn initial_assignment_orders_by_release_date_then_publisher_code() {
        let rows = vec![row(1, "zzz"), row(2, "aaa"), row(3, "old")];
        let metadata = HashMap::from([
            ("zzz".to_string(), set(1, "zzz", "2025-02-01")),
            ("aaa".to_string(), set(2, "aaa", "2025-02-01")),
            ("old".to_string(), set(3, "old", "1993-08-05")),
        ]);
        let assigned = assign_sets(&rows, &metadata, Vec::new()).unwrap();
        assert_eq!(
            assigned
                .iter()
                .map(|entry| entry.anonymous_id.as_str())
                .collect::<Vec<_>>(),
            ["1993A", "2025A", "2025B"]
        );
        assert_eq!(
            assigned.iter().map(|entry| entry.set_id).collect::<Vec<_>>(),
            [uuid(3), uuid(2), uuid(1)]
        );
    }

    #[test]
    fn frozen_assignments_never_move_when_an_older_set_appears_late() {
        let rows = vec![row(1, "late")];
        let metadata = HashMap::from([("late".to_string(), set(2, "late", "2025-01-01"))]);
        let existing = vec![SetAssignment {
            anonymous_id: "2025A".to_string(),
            set_id: uuid(1),
        }];
        let assigned = assign_sets(&rows, &metadata, existing).unwrap();
        assert_eq!(assigned[0].anonymous_id, "2025A");
        assert_eq!(assigned[1].anonymous_id, "2025B");
    }

    #[test]
    fn suffix_continues_from_z_to_aa() {
        assert_eq!(alpha_suffix(0), "A");
        assert_eq!(alpha_suffix(25), "Z");
        assert_eq!(alpha_suffix(26), "AA");
        assert_eq!(alpha_suffix(27), "AB");
        assert_eq!(parse_anonymous_set_id("2025AA").unwrap(), (2025, 26));
    }

    #[test]
    fn rendered_catalog_contains_no_publisher_code_or_title() {
        let rows = vec![row(1, "set")];
        let metadata = HashMap::from([("set".to_string(), set(7, "set", "2025-01-01"))]);
        let assignments = vec![SetAssignment {
            anonymous_id: "2025A".to_string(),
            set_id: uuid(7),
        }];
        let output = render_catalog(&rows, &metadata, &assignments).unwrap();
        assert!(output.starts_with(CATALOG_HEADER));
        assert!(output.ends_with("\t2025A\n"));
        assert!(!output.contains("\tset"));
    }
}
