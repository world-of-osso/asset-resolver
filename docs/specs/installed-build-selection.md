# Installed build selection

The CASC resolver selects the requested local product build from `.product.db` before reading its build configuration. The resolver contract is implemented in [`src/casc_resolver.rs`](../../src/casc_resolver.rs).

## What it must do

- [x] Select exactly `WOW_PRODUCT`, defaulting to `wow`; do not select another product's active row.
- [x] Use the selected product's `baseProductState.activeBuildKey` (protobuf field 14) to locate `Data/config/<key-prefix>/<build-key>`.
- [x] Surface a missing requested product or malformed/missing `.product.db`; do not fall back to `.build.info`.
- [x] Read installation metadata without writing it.
- [x] Treat field-14 selection as a deterministic resolver policy, not a claim of native Battle.net selection semantics.
- [x] Leave completeness and local config/archive availability to the readers that need those assets.

### Explicit startup initialization

- [ ] Expose `CascListfileResolver::initialize() -> Result<(), String>` so callers can eagerly initialize local CASC before extracting an asset. Call it from a worker thread: loading keys, resolution tables and installation indices can take seconds.
- [ ] Reuse process-wide state across resolver instances; the first CASC caller's paths determine that state. Repeated initialization returns the cached success or failure, without retrying initialization or switching configuration.
- [ ] Report bootstrap failures by logging their underlying cause and returning `CASC not available`; return and cache installation-initialization errors as `CASC init: ...`. Without the `casc` feature, return `asset-resolver was built without the casc feature`.
- [ ] Initialize without resolving an asset FDID, loading the resolver's listfile, or writing an extracted asset. Resolution-cache generation may still write metadata; success does not prove any particular asset is readable, and direct encoding-key archive access remains lazy.

These startup requirements are source-audited at `6fcab75`, not test-verified by this documentation audit.

## How it works

- [`src/casc_resolver.rs`](../../src/casc_resolver.rs) obtains product metadata through `cascette-client-storage` and opens the resulting build configuration.
- [`cascette-client-storage` metadata contract](https://github.com/Osso/cascette-rs/tree/fix-local-index-generations/crates/cascette-client-storage) defines the bounded `.product.db` read.

## Implementation inventory

- `src/casc_resolver.rs` — resolves the requested product, opens its build configuration, and initializes shared CASC state.
- `src/lib.rs` — public `CascListfileResolver::initialize` startup API.

## Tests asserting this spec

- `src/casc_resolver.rs` — `requested_forever_build_is_selected_without_build_info_row`.
- `src/casc_resolver.rs` — `missing_requested_product_errors_instead_of_using_retail`.

- `tests/initialize.rs` — local-install success, repeated-call timing, subsequent SoundKit extraction, and feature-disabled error. This audit did not run these tests; they do not assert failure caching or the no-extraction boundary.

## Known gaps (current cycle)

- None.

## Out of scope

- Matching native Battle.net selection behavior.
- Validating that the selected installation's local CASC assets are complete or readable.
