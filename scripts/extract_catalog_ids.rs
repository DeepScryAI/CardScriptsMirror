#!/usr/bin/env rust-script
//! ```cargo
//! [package]
//! edition = "2021"
//!
//! [dependencies]
//! anyhow = "1.0.99"
//! clap = { version = "4.5.45", features = ["derive"] }
//! sha2 = "0.10.9"
//! uuid = "1.18.0"
//! ```

use anyhow::{bail, Context, Result};
use clap::Parser;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(about = "Extract the anonymous numeric/Oracle identity bridge from DeepScry's catalog")]
struct Args {
    /// DeepScry card_catalog.tsv containing id, name, and oracle_id columns.
    #[arg(long)]
    source: PathBuf,

    /// Anonymous output consumed by generate_uuid_trie.rs.
    #[arg(long, default_value = "catalog_ids.tsv")]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let source =
        fs::read_to_string(&args.source).with_context(|| format!("read DeepScry catalog {}", args.source.display()))?;
    let anonymous = extract_catalog(&source)?;
    let temporary = args.output.with_extension("write-part");
    fs::write(&temporary, anonymous).with_context(|| format!("write temporary catalog {}", temporary.display()))?;
    fs::rename(&temporary, &args.output)
        .with_context(|| format!("publish anonymous catalog {}", args.output.display()))?;
    eprintln!("Wrote {}", args.output.display());
    Ok(())
}

fn extract_catalog(source: &str) -> Result<String> {
    let mut lines = source.lines();
    let header = lines.next().context("DeepScry catalog is empty")?;
    let columns: Vec<&str> = header.split('\t').collect();
    let id_column = column_index(&columns, "#id")?;
    let name_column = column_index(&columns, "name")?;
    let first_set_column = column_index(&columns, "first_set")?;
    let oracle_id_column = column_index(&columns, "oracle_id")?;

    let mut output = String::from("#id\toracle_id\tname_sha256\tset_group\n");
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
        let first_set = field(&fields, first_set_column, line_number, "first_set")?;
        let set_group = anonymous_set_group(first_set);
        if !identities.insert((oracle_id, name_hash.clone())) {
            bail!("line {line_number} duplicates an Oracle UUID/name identity");
        }
        output.push_str(&format!("{id}\t{}\t{name_hash}\t{set_group}\n", oracle_id.hyphenated()));
    }
    if ids.is_empty() {
        bail!("DeepScry catalog contains no card rows");
    }
    Ok(output)
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

fn anonymous_set_group(set_code: &str) -> String {
    format!(
        "G{}",
        &hex_sha256(set_code.trim().to_ascii_uppercase().as_bytes())[..16]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_identity_and_one_way_name_digest() {
        let source = "#id\tname\tfirst_set\toracle_id\tgeneration\tflags\n1\tExample Card\tset\t12345678-1234-1234-1234-123456789abc\t1\t\n";
        let output = extract_catalog(source).unwrap();
        assert!(output.starts_with("#id\toracle_id\tname_sha256\tset_group\n1\t12345678-1234-1234-1234-123456789abc\t"));
        assert!(!output.contains("Example Card"));
        assert!(output.ends_with("\tG2992d15897b5bbe7\n"));
    }

    #[test]
    fn rejects_duplicate_numeric_ids() {
        let source = "#id\tname\toracle_id\n1\tA\t12345678-1234-1234-1234-123456789abc\n1\tB\t22345678-1234-1234-1234-123456789abc\n";
        assert!(extract_catalog(source).is_err());
    }
}
