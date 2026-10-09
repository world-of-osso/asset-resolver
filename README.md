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

CASC initialization selects the requested build config, not the active product
or `WOW_PRODUCT`. States and cached failures are separate per cache root and
identity. Missing matching configs, root entries or decryption keys error; an
explicit-identity FDID cannot borrow the installation's listfile-path lookup.
Consumers must retain the namespace in parsed-model, decoded-texture and GPU
material keys. This library does not infer a model's author from race or FDID.

Existing non-model consumers without an identity retain their current interface.
The game-engine full-chain migration is not implemented by this API alone; its
approved contract lives in `docs/specs/product-isolated-model-assets.md` in the
engine repository. Depot stages a branch checkout using
`DEPOT_SIBLING_ASSET_RESOLVER=/home/osso/.worktrees/asset-resolver`.

## Focused tests

`cargo test --locked --test product_identity` exercises real scoped cache reads,
product/build coexistence, companion-name namespaces, legacy rejection and
qualified-path ownership. Payload receipts are not M2/BLP parsing or GPU proof.
`cargo test --locked --lib authored_build` exercises pinned config selection;
`cargo test --locked --lib missing_authored_build` tests unavailable build errors.
