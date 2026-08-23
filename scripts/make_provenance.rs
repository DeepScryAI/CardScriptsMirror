#!/usr/bin/env rust-script
//! ```cargo
//! [package]
//! edition = "2021"
//!
//! [dependencies]
//! anyhow = "1.0.99"
//! clap = { version = "4.5.45", features = ["derive"] }
//! serde_json = "1.0.143"
//! sha2 = "0.10.9"
//! uuid = "1.18.0"
//! ```
//!
//! Emit the SS2 **provenance table**: dense `id → oracle_id` rows in the
//! shared self-verifying TSV envelope, from this repository's
//! `catalog_ids.tsv`.
//!
//! Normative format: `docs/CARD_SKIN_FORMATS.md` in the DeepScry repository
//! (ds-5432). Provenance is deliberately a SKIN-manifest member, never a
//! cardset member: Scryfall oracle ids are worldly identity, and cardsets
//! are anonymous. The Wizards skin carries this table (it feeds oracle-id
//! keyed tooling and the extraction pipeline); custom skins omit it.

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[path = "lib/cas.rs"]
mod cas;

#[derive(Parser, Debug)]
#[command(about = "Emit the dense id -> oracle_id provenance TSV (SS2) and print its CID")]
struct Args {
    /// The numeric identity bridge (`#id` and `oracle_id` columns).
    #[arg(long, default_value = "catalog_ids.tsv")]
    catalog: PathBuf,

    /// The `catalog_identity` stamp: the SHA-256 of the catalog file that
    /// assigned these numeric ids (DeepScry's embedded card_catalog.tsv).
    /// Required explicitly because `catalog_ids.tsv` is a derived bridge,
    /// not the identity-assigning file itself.
    #[arg(long)]
    catalog_identity: String,

    /// Output table.
    #[arg(long, default_value = "presentation/provenance_oracle_ids.tsv")]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.catalog_identity.len() != 64
        || !args.catalog_identity.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("--catalog-identity must be a lowercase hex SHA-256");
    }

    let text = fs::read_to_string(&args.catalog).with_context(|| format!("read {}", args.catalog.display()))?;
    let mut lines = text.lines();
    let header = lines.next().context("catalog has no header row")?;
    let columns: Vec<&str> = header.split('\t').collect();
    let id_column = columns
        .iter()
        .position(|c| *c == "#id")
        .context("catalog header has no #id column")?;
    let oracle_column = columns
        .iter()
        .position(|c| *c == "oracle_id")
        .context("catalog header has no oracle_id column")?;

    let mut body = String::new();
    let mut expected: u32 = 0;
    for (offset, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_number = offset + 2;
        let fields: Vec<&str> = line.split('\t').collect();
        let id: u32 = fields
            .get(id_column)
            .with_context(|| format!("catalog line {line_number} has no #id"))?
            .parse()
            .with_context(|| format!("catalog line {line_number} has a non-numeric #id"))?;
        let oracle_id: Uuid = fields
            .get(oracle_column)
            .with_context(|| format!("catalog line {line_number} has no oracle_id"))?
            .parse()
            .with_context(|| format!("catalog line {line_number} has an invalid oracle_id"))?;
        expected += 1;
        if id != expected {
            bail!("catalog is not dense: expected id {expected} at line {line_number}, found {id}");
        }
        body.push_str(&format!("{id}\t{oracle_id}\n"));
    }
    if expected == 0 {
        bail!("catalog {} has no rows", args.catalog.display());
    }

    let header_line = format!(
        "#id\toracle_id\tmetadata: v=1 kind=provenance-oracle-ids catalog_identity={} cards={} body_sha256={}\n",
        args.catalog_identity,
        expected,
        cas::sha256_hex(body.as_bytes()),
    );
    let mut document = header_line.into_bytes();
    document.extend_from_slice(body.as_bytes());

    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let temporary = args.output.with_extension("write-part");
    fs::write(&temporary, &document).with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, &args.output).with_context(|| format!("publish {}", args.output.display()))?;

    println!("provenance_cid={}", cas::cid_for_bytes(&document));
    println!("provenance_size={}", document.len());
    println!("rows={expected}");
    eprintln!("Wrote {}", args.output.display());
    Ok(())
}
