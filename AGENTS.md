# CardScriptsMirror agent guide

This repository is not a copy of the old card-script folder. It is the
scrubbed producer boundary for card presentation data and art assets.

## Ownership boundary

CardScriptsMirror alone may interact with Scryfall. It downloads and dissects
Scryfall bulk metadata dumps, then produces card skins and artpacks. Keep that
work here.

DeepScry is a consumer only: it must not call Scryfall, parse Scryfall bulk
dumps, or carry that metadata. It receives a skin or artpack by its content
hash and a URL. This separation is part of the intellectual-property scrub,
not merely repository organization. Do not add a Scryfall call or bulk-dump
parser back to DeepScry as a convenience.

## Generated skins and artpacks

A card skin and an artpack use one file format: a gzipped tab-separated-value
(TSV) file identified by the hash of its complete bytes. Generated output is
gitignored and belongs in the same directory as the generator; commit the
generator and reproducible source inputs, never generated pack output.

Ship a presentation skin as a three-part bundle:

1. titles;
2. body text; and
3. artpack.

All three parts travel the same way: content hashes with URL pointers into
static storage. A browser loads an account-selected URL directly; the game
server does not proxy or inspect it.

Artpack rows deliberately keep their first two fields identical in both
encodings:

```text
skinny: catalog_id<TAB>url
fat:    catalog_id<TAB>url<TAB>key[<TAB>label...]
```

The `key` is after the URL. Readers that only need one image can therefore use
the first row for a catalog id and ignore later fields, while richer clients can
use the optional key and human-readable labels for an art picker. Sort rows by
`catalog_id`.

Do not create a second producer elsewhere. If a consumer needs a new field or
format capability, evolve the pack contract here and keep DeepScry consuming
only the content-addressed result.
