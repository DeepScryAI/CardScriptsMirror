#!/usr/bin/env python3
"""Run card-compiler's shared scanner against the executable corpus only.

Fetched source inputs and the WotC presentation skin intentionally contain IP;
these are outside the cards/ and tokens/ scopes, never scanner exceptions.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def check(root: Path, compiler: Path, cache: Path, reports: Path) -> None:
    expected = (root / ".card-compiler-revision").read_text().strip()
    actual = subprocess.check_output(
        ["git", "-C", str(compiler), "rev-parse", "HEAD"], text=True
    ).strip()
    if actual != expected:
        raise RuntimeError(f"card-compiler revision mismatch: expected {expected}, got {actual}")
    dirty = subprocess.check_output(
        ["git", "-C", str(compiler), "status", "--porcelain", "--", "scripts", "ip_allowlist.tsv"], text=True
    ).strip()
    if dirty:
        raise RuntimeError("card-compiler scanner checkout has uncommitted changes")
    scanner = compiler / "scripts/scan_scryfall_ip.rs"
    allowlist = compiler / "ip_allowlist.tsv"
    if not scanner.is_file() or not allowlist.is_file():
        raise RuntimeError("card-compiler scanner or shared policy is missing")
    reports.mkdir(parents=True, exist_ok=True)
    # A private directory makes an old report incapable of certifying this run.
    with tempfile.TemporaryDirectory(prefix="ip-check-", dir=reports) as temporary:
        for prefix in ("cards", "tokens"):
            report = Path(temporary) / f"{prefix}.json"
            command = ["rust-script", str(scanner), "--target", "card-scripts-mirror",
                       "--root", str(root), "--exclude-submodules", "--path-prefix", prefix,
                       "--cache", str(cache), "--allowlist", str(allowlist),
                       "--report", str(report)]
            result = subprocess.run(command, check=False)
            if not report.is_file():
                raise RuntimeError(f"{prefix}: scanner did not produce a fresh report (exit {result.returncode})")
            data = json.loads(report.read_text())
            destination = reports / f"nightly-ip-{prefix}.json"
            destination.write_text(json.dumps(data, indent=2) + "\n")
            if result.returncode != 0:
                raise RuntimeError(f"{prefix}: scanner rejected the corpus (exit {result.returncode}); see {destination}")
            if data.get("target") != "card-scripts-mirror" or data.get("submodules_included") is not False:
                raise RuntimeError(f"{prefix}: unexpected scanner scope")
            if not isinstance(data.get("text_files_scanned"), int) or data["text_files_scanned"] <= 0:
                raise RuntimeError(f"{prefix}: empty scan cannot certify the corpus")
            if data.get("hit_pairs") != 0 or data.get("omitted_hit_pairs") != 0:
                raise RuntimeError(f"{prefix}: IP findings remain; see {destination}")
            print(f"IP gate: {prefix}: {data['text_files_scanned']} text files, zero hit pairs")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", type=Path, default=os.environ.get("CARD_COMPILER_DIR"))
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--reports", type=Path, required=True)
    args = parser.parse_args()
    if args.compiler is None:
        parser.error("set CARD_COMPILER_DIR or pass --compiler (an independent card-compiler checkout)")
    root = Path(__file__).resolve().parents[1]
    check(root, args.compiler.resolve(), args.cache.resolve(), args.reports.resolve())


if __name__ == "__main__":
    main()
