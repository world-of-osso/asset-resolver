# Installed build selection

The CASC resolver selects the requested local product build from `.product.db` before reading its build configuration. The resolver contract is implemented in [`src/casc_resolver.rs`](../../src/casc_resolver.rs).

## What it must do

- [x] Select exactly `WOW_PRODUCT`, defaulting to `wow`; do not select another product's active row.
- [x] Use the selected product's `baseProductState.activeBuildKey` (protobuf field 14) to locate `Data/config/<key-prefix>/<build-key>`.
- [x] Surface a missing requested product or malformed/missing `.product.db`; do not fall back to `.build.info`.
- [x] Read installation metadata without writing it.
- [x] Treat field-14 selection as a deterministic resolver policy, not a claim of native Battle.net selection semantics.
- [x] Leave completeness and local config/archive availability to the readers that need those assets.

## How it works

- [`src/casc_resolver.rs`](../../src/casc_resolver.rs) obtains product metadata through `cascette-client-storage` and opens the resulting build configuration.
- [`cascette-client-storage` metadata contract](https://github.com/Osso/cascette-rs/tree/fix-local-index-generations/crates/cascette-client-storage) defines the bounded `.product.db` read.

## Implementation inventory

- `src/casc_resolver.rs` — resolves the requested product and opens its build configuration.

## Tests asserting this spec

- `src/casc_resolver.rs` — `requested_forever_build_is_selected_without_build_info_row`.
- `src/casc_resolver.rs` — `missing_requested_product_errors_instead_of_using_retail`.

## Known gaps (current cycle)

- None.

## Out of scope

- Matching native Battle.net selection behavior.
- Validating that the selected installation's local CASC assets are complete or readable.
