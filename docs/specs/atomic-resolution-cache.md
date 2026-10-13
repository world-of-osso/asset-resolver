# Atomic CASC resolution namespaces

## Requirements

- A missing `<cache>/casc/<product>/<build-key>/schema-2` is built lazily, without a manual refresh step.
- Concurrent processes serialize namespace creation with a persistent sibling lock file (`flock`). Exactly one builds; waiting callers reuse the complete namespace.
- Root, encoding and committed SQLite metadata are built in a private sibling directory. Rename publishes the complete directory. Published complete namespaces are immutable; `casc_refresh` also reuses them.
- Failed builders publish nothing. Killed builders release their lock through process exit; their private directories are ignored by later builders and readers.
- Truncated, invalid or content-key-mismatched metadata fails explicitly. No incomplete namespace is published.
- Freshness inspection never deletes SQLite databases or sidecars.

## Proof

`tests/follow_installed_build.rs`: twelve real concurrent reader processes against a synthetic CASC install, killed private publisher followed by real-install recovery, mid-patch root metadata rejection, and truncated encoding archive rejection. `src/casc_cache.rs` also verifies freshness inspection preserves databases and transaction sidecars. Existing patch-transition test covers separate build namespaces.

## Exclusions

Install repair, canonical data mutation, and orphan-directory garbage collection.
