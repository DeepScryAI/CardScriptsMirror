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
//! ```
//!
//! Assemble the SS2 **skin manifest**: the single small canonical-JSON
//! object whose CID names a whole card skin.
//!
//! Normative format: `docs/CARD_SKIN_FORMATS.md` in the DeepScry repository
//! (ds-5432). The manifest binds a mandatory cardset reference (the
//! identity-conferring binding) and a required titles reference, plus
//! optional bodies / artpack / provenance references. Every reference is
//! the `{cid, size, hints[]}` triple computed from the actual artifact
//! bytes; hint URLs are INSIDE the manifest's hash (pure content
//! addressing — editing hints mints a new manifest).

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "lib/cas.rs"]
mod cas;

#[derive(Parser, Debug)]
#[command(about = "Assemble the SS2 skin manifest from artifact files and print every CID")]
struct Args {
    /// The SS1 cardset tarball (required — the identity-conferring binding).
    #[arg(long)]
    cardset: PathBuf,

    /// The SS3 titles table (required).
    #[arg(long)]
    titles: PathBuf,

    /// The SS4 bodies table (optional).
    #[arg(long)]
    bodies: Option<PathBuf>,

    /// The SS5 artpack table (optional).
    #[arg(long)]
    artpack: Option<PathBuf>,

    /// The id → oracle_id provenance table (optional; Wizards skin only).
    #[arg(long)]
    provenance: Option<PathBuf>,

    /// Retrieval hints, repeatable, as `<member>=<url>` where `<member>` is
    /// one of cardset|titles|bodies|artpack|provenance. Hints are inside
    /// the manifest's hash.
    #[arg(long = "hint")]
    hints: Vec<String>,

    /// Output file (canonical JCS bytes).
    #[arg(long, default_value = ".cache/cas/skin_manifest.json")]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let mut hints_by_member: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    for hint in &args.hints {
        let (member, url) = hint
            .split_once('=')
            .context("--hint must be <member>=<url>")?;
        if !matches!(member, "cardset" | "titles" | "bodies" | "artpack" | "provenance") {
            bail!("--hint member {member:?} must be cardset|titles|bodies|artpack|provenance");
        }
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            bail!("--hint url {url:?} must be absolute http(s)");
        }
        hints_by_member.entry(member).or_default().push(url.to_owned());
    }

    let mut manifest = serde_json::Map::new();
    manifest.insert("format".to_owned(), "deepscry-card-skin".into());
    manifest.insert("version".to_owned(), 1.into());
    let mut member_lines: Vec<String> = Vec::new();
    let mut add = |name: &str, path: &Path, manifest: &mut serde_json::Map<String, serde_json::Value>| -> Result<()> {
        let bytes = fs::read(path).with_context(|| format!("read {name} artifact {}", path.display()))?;
        let hints = hints_by_member.get(name).cloned().unwrap_or_default();
        let reference = cas::content_ref(&bytes, &hints);
        member_lines.push(format!(
            "{name}_cid={} {name}_size={}",
            reference["cid"].as_str().expect("cid is a string"),
            bytes.len(),
        ));
        manifest.insert(name.to_owned(), reference);
        Ok(())
    };
    add("cardset", &args.cardset, &mut manifest)?;
    add("titles", &args.titles, &mut manifest)?;
    if let Some(path) = &args.bodies {
        add("bodies", path, &mut manifest)?;
    }
    if let Some(path) = &args.artpack {
        add("artpack", path, &mut manifest)?;
    }
    if let Some(path) = &args.provenance {
        add("provenance", path, &mut manifest)?;
    }
    for member in hints_by_member.keys() {
        if !manifest.contains_key(*member) {
            bail!("--hint given for {member} but no --{member} artifact was supplied");
        }
    }

    let document = cas::jcs_canonicalize(&serde_json::Value::Object(manifest))
        .context("canonicalize skin manifest")?;
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let temporary = args.output.with_extension("write-part");
    fs::write(&temporary, &document).with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, &args.output).with_context(|| format!("publish {}", args.output.display()))?;

    println!("skin_manifest_cid={}", cas::cid_for_bytes(&document));
    println!("skin_manifest_size={}", document.len());
    for line in member_lines {
        println!("{line}");
    }
    eprintln!("Wrote {}", args.output.display());
    Ok(())
}
