# Local asset file cache

`CascListfileResolver::ensure_cached` serves cached files or extracts FileDataIDs from local CASC. Its implementation is in `src/lib.rs`, `src/paths.rs`, and `src/casc_resolver.rs`.

## What it must do

- [x] Return existing cached files unchanged.
- [x] Extract an available local asset despite a persisted `.missing` marker, including in a fresh process.
- [x] Preserve configured source/shared destination routing and acquired bytes.
- [x] Return `None` for unavailable assets without creating a positive output, logging the FileDataID, destination, and cause.

## How it works

- [Engine asset pipeline](../../../game-engine/docs/wiki/systems/asset-pipeline.md).

## Implementation inventory

- `src/lib.rs` — public resolver API.
- `src/paths.rs` — source/shared destination routing.
- `src/casc_resolver.rs` — positive file cache and local extraction.

## Tests asserting this spec

- `tests/negative_cache.rs` — fresh-process marker recovery, positive-cache preservation, and unavailable-asset diagnostics using local CASC.

## Known gaps (current cycle)

No open cache-repair gaps in this cycle. Independent checks and the full default-CASC package suite pass; native cold-cache rendering and Rust 1.89 compatibility are not claimed.

## Out of scope

- Resolution-table generation, archive initialization, and renderer changes: unaffected by this cache repair.
- Blanket cache cleanup: user assets and historical markers remain untouched.
