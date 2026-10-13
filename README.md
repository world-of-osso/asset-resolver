# asset-resolver

Local CASC and listfile lookup. No CDN acquisition.

## Authored model assets

Create `AssetIdentity::new(product, build_key)` from verified importer provenance,
then pass it to `AssetResolverConfig::with_identity`. Supported products: `wow`
and `wow_classic_beta`; build keys are 32 hexadecimal characters.

`CascListfileResolver::ensure_cached_checked(fdid, destination)` returns the actual
qualified cache path or the extraction error. With an identity, a destination
`<data>/models/1100087.m2` maps to
`<shared-data>/products/<product>/<build-key>/models/1100087.m2`; skins, skeletons,
animations and textures use the same namespace. Already qualified paths must
match the requested identity. Unqualified files are never read or relabelled.

Local CASC extraction selects the product's unique active `.build.info` row,
never the receipt's former build key or `.product.db`. `WOW_PRODUCT` selects the
product for unqualified requests; explicit identities select their own product.
States, failures, root/encoding files and resolution SQLite are keyed by cache
root + installed product/build (`casc/<product>/<build-key>/schema-2/`). A patch
opens a new state/cache even within one process. Legacy unqualified tables are
not reused. Missing configs, root entries or decryption keys error.

Existing qualified files retain their historical identity. A miss extracts into
the currently installed build's namespace and returns that actual path; it never
labels newly extracted bytes with the old receipt key. Extracted-only behavior
is unchanged and never consults the install.
Consumers must retain the namespace in parsed-model, decoded-texture and GPU
material keys. This library does not infer a model's author from race or FDID.

Legacy `ensure_cached` and `ensure_file_cached_at_path` return
`Result<Option<PathBuf>, String>`. Local-CASC hits return `Ok(Some(path))` and
extraction failures still log and return `Ok(None)`. Extracted-only misses return
exactly the checked API's `Err(String)` (mode/FDID/authored product-build/path),
without panicking. Callers must propagate this error through worker completions
or log it explicitly. Ordinary extracted-file misses do not enter CASC; the
forbidden-entry counter and hook ordering are unchanged.
The game-engine full-chain migration is not implemented by this API alone; its
approved contract lives in `docs/specs/product-isolated-model-assets.md` in the
engine repository. Depot stages a branch checkout using
`DEPOT_SIBLING_ASSET_RESOLVER=/home/osso/.worktrees/asset-resolver`.

## Frozen offline extraction

`FrozenArchiveReader::open(archive_dir, key_file)` reads local archives by encoding
key without active install/config/root selection. Offline importers authenticate
a frozen product/build resolution snapshot, obtain its encoding keys, and verify
each returned payload against its content key before publishing. This permits
continued extraction after an install update removes the old build config.
Missing archive locations and encrypted chunks error; zero-filled bytes are never
accepted. Existing CASC guards forbid this reader in extracted-only runtime mode.
It is an explicit offline source, not a runtime or legacy fallback.

## Focused tests

`cargo test --locked --test product_identity` exercises real scoped cache reads,
product/build coexistence, companion-name namespaces, legacy rejection and
qualified-path ownership. Payload receipts are not M2/BLP parsing or GPU proof.
`cargo test --locked --lib installed_build_tests` checks active-row and product
selection. `cargo test --locked --test follow_installed_build` creates real local
indices/archives, bootstraps tables, patches `.build.info` in-process and proves
new bytes/new cache namespace plus preservation of historical published bytes.
