# TEMPORARY transition scaffolding — not the intended end state

This branch exists only to carry the identity-scrub transition. It deliberately
contains WotC card titles in a tracked file, which the clean primary
`codex/ip-clean-numeric-mirror-v2` must never do. Do not merge this branch into
the clean primary, and do not treat it as the permanent home of this artifact.

`title_catalog.tsv` is the dense 35,307-row `CardCatalogId`-to-title table that
DeepScry's temporary title-skin generator consumes. It is generated output,
committed here — against this repository's normal rule that generated packs stay
gitignored — because the transition needs a fixed, checkout-able artifact while
the test suite still depends on real titles.

## What retires this branch

The staged plan, in order:

1. Local games run with this skin, producing output identical to the pre-scrub
   build, with the full suite green.
2. Network games run with this same skin on both sides.
3. The suite is weaned onto the degenerate skin — bare `CARD#123` identifiers,
   or the constant-title `A_CARD_NAME` skin — so that passing no longer depends
   on real titles.
4. Only then, network games with different skins on each side.

**Delete this branch once step 3 holds.** Its whole justification is that the
suite cannot yet pass without real titles. Once it can, a tracked table of WotC
titles is pure liability. Until then, do not delete it: nothing else in the
clean lineage carries a checkout-able copy.

## Reproducing it

The table is not hand-maintained. Regenerate it with the branch's own generator:

```sh
./scripts/generate_title_catalog.rs \
  --catalog /path/to/DeepScry/src/engine/assets/card_catalog.tsv \
  --output presentation/title_catalog.tsv
```

Titles come from this repository's Scryfall snapshot, joined to the catalog by
Oracle ID. The catalog supplies only `#id` and `oracle_id`; its `name` column is
never read. Verify a table against a catalog with `--verify`.

## The stamp expires when the catalog is scrubbed

`catalog_identity` is the SHA-256 of the *entire* catalog file. Removing the
`name` column from DeepScry's `card_catalog.tsv` changes those bytes, so it
changes the identity, and this table's stamp will no longer match. Consumers
will then reject it — correctly, because that is exactly the stale-table
protection the stamp exists to provide.

The title rows themselves are unaffected: regenerating from the scrubbed catalog
reproduces all 35,307 titles byte-for-byte, because they come from Scryfall
rather than from the catalog. Only the header's identity changes.

**So this file must be re-emitted in the same change that scrubs the catalog.**
If the scrub lands without it, skin loading breaks at that commit and the
failure will look unrelated to the scrub.

## Byte-identical to the previously pinned table

This table matches the previously pinned mirror table at `8610a5039` exactly —
all 35,307 rows and the header. There is no delta to reason about when
comparing against pre-scrub output.

An earlier revision of this branch differed at catalog ID 31026, emitting
`Mechtitan` where the pinned table said `Mechtitan // Mechtitan`, and this file
wrongly described the doubled form as an artifact of the older pipeline. It is
not: it is Scryfall's genuine `reversible_card` name. The generator had been
joining on Scryfall's top-level `oracle_id` only, and all 81 `reversible_card`
printings carry a null top-level id with their identity on the faces, so it
never saw them. Catalog ID 31026 denotes the Secret Lair reversible printing;
the generator had resolved its Oracle id to an unrelated Neon Dynasty token
that shares it.
