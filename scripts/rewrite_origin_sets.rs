#!/usr/bin/env rust-script
//! ```cargo
//! [package]
//! edition = "2021"
//!
//! [dependencies]
//! anyhow = "1.0.99"
//! clap = { version = "4.5.45", features = ["derive"] }
//! ```

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::fs;
use std::path::{Path, PathBuf};

const CATALOG_HEADER: &str = "#id\toracle_id\tname_sha256\tanonymous_set_id";

#[derive(Debug, Parser)]
#[command(about = "Rewrite only OriginSet fields in an existing numeric card trie")]
struct Args {
    #[arg(long, default_value = "catalog_ids.tsv")]
    catalog: PathBuf,
    #[arg(long, default_value = "cards")]
    cards: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let catalog = fs::read_to_string(&args.catalog)
        .with_context(|| format!("read anonymous catalog {}", args.catalog.display()))?;
    let assignments = parse_assignments(&catalog)?;
    let mut rewritten = 0usize;
    for (id, origin_set) in assignments {
        let path = card_path(&args.cards, id);
        if !path.exists() {
            continue;
        }
        let source = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let output =
            rewrite_origin_set(&source, id, &origin_set).with_context(|| format!("rewrite {}", path.display()))?;
        if output != source {
            let temporary = path.with_extension("write-part");
            fs::write(&temporary, output).with_context(|| format!("write {}", temporary.display()))?;
            fs::rename(&temporary, &path).with_context(|| format!("publish {}", path.display()))?;
            rewritten += 1;
        }
    }
    eprintln!("Rewrote {rewritten} OriginSet fields beneath {}", args.cards.display());
    Ok(())
}

fn parse_assignments(source: &str) -> Result<Vec<(u32, String)>> {
    let mut lines = source.lines();
    if lines.next() != Some(CATALOG_HEADER) {
        bail!("unexpected anonymous catalog header");
    }
    let mut assignments = Vec::new();
    let mut expected_id = 1u32;
    for (offset, line) in lines.enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 4 {
            bail!("anonymous catalog line {} does not have four columns", offset + 2);
        }
        let id: u32 = fields[0]
            .parse()
            .with_context(|| format!("invalid card ID on catalog line {}", offset + 2))?;
        if id != expected_id {
            bail!("catalog line {} has ID {id}, expected {expected_id}", offset + 2);
        }
        validate_origin_set(fields[3])?;
        assignments.push((id, fields[3].to_string()));
        expected_id += 1;
    }
    Ok(assignments)
}

fn card_path(root: &Path, id: u32) -> PathBuf {
    let key = format!("{id:08}");
    root.join(&key[0..2])
        .join(&key[2..4])
        .join(&key[4..6])
        .join(format!("{key}.txt"))
}

fn rewrite_origin_set(source: &str, id: u32, origin_set: &str) -> Result<String> {
    validate_origin_set(origin_set)?;
    let mut output = String::with_capacity(source.len());
    let mut id_matches = 0usize;
    let mut origin_matches = 0usize;
    let expected_id = format!("Id:{id}");
    for line in source.lines() {
        if line == expected_id {
            id_matches += 1;
        }
        if line.starts_with("OriginSet:") {
            origin_matches += 1;
            output.push_str("OriginSet:");
            output.push_str(origin_set);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if id_matches != 1 || origin_matches != 1 {
        bail!("expected one Id:{id} and one OriginSet field, found {id_matches} and {origin_matches}");
    }
    Ok(output)
}

fn validate_origin_set(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() < 5 || !bytes[..4].iter().all(u8::is_ascii_digit) || !bytes[4..].iter().all(u8::is_ascii_uppercase) {
        bail!("invalid YEAR+LETTER origin-set ID {value:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_only_the_origin_set_field() {
        let source = "Id:3\nColorIdentity:R\nOriginSet:Gfixture\nManaCost:R\n";
        assert_eq!(
            rewrite_origin_set(source, 3, "2025AA").unwrap(),
            "Id:3\nColorIdentity:R\nOriginSet:2025AA\nManaCost:R\n"
        );
    }

    #[test]
    fn rejects_missing_or_duplicate_structural_fields() {
        assert!(rewrite_origin_set("Id:3\nManaCost:R\n", 3, "2025A").is_err());
        assert!(rewrite_origin_set("Id:3\nOriginSet:x\nOriginSet:y\n", 3, "2025A").is_err());
    }

    #[test]
    fn numeric_trie_path_is_stable() {
        assert_eq!(
            card_path(Path::new("cards"), 12_345_678),
            PathBuf::from("cards/12/34/56/12345678.txt")
        );
    }
}
