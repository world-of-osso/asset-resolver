# Listfile resolution

The asset resolver maps WoW FileDataIDs and normalized asset paths through local and community listfile caches. Source lives in `src/listfile.rs` and `src/listfile_cache.rs`.

## What it must do

### Initialization and lookup

- [x] Construct a `Listfile` without preloading the local SQLite cache into memory.
- [x] Resolve FileDataIDs from the local SQLite cache on an in-memory miss and populate both in-memory indexes.
- [x] Resolve paths case-insensitively from the local SQLite cache on an in-memory miss and populate both in-memory indexes.
- [x] Prefer local SQLite entries over conflicting community listfile entries.
- [x] Treat a missing local SQLite file or missing local table as an empty local cache.

### Persistence and fallback

- [x] Persist community-resolved entries into the local SQLite cache and in-memory indexes.
- [x] Fall back to the community listfile only after a local miss.
- [x] Preserve configurable source, shared-data, and extraction-cache roots.

## How it works

- `src/listfile.rs` — lookup order, in-memory indexes, lazy local access, and community fallback.
- `src/listfile_cache.rs` — SQLite queries, persistence, and community-cache generation.
- `src/paths.rs` — resolver path configuration.

## Implementation inventory

- `src/listfile.rs` — public Listfile construction and lookup behavior.
- `src/listfile_cache.rs` — local/community SQLite storage.
- `src/lib.rs` — resolver API and global lazy resolver.

## Tests asserting this spec

- `src/listfile.rs` — lazy local FDID/path lookup, cache population, missing-table behavior, and local priority.
- `src/paths.rs` — configurable path roots.
- `src/lib.rs` — resolver construction with explicit locations.

## Known gaps (current cycle)

- [ ] Re-run the game-engine full project checks against this local dependency.
- [ ] Publish a fetchable commit and update wow-ui-sim's pinned Git revision after explicit push authorization.

## Out of scope

- Changing CASC extraction, cache locations, community listfile import format, or lookup results.
- Permanently patching consumers to a machine-local asset-resolver path.
