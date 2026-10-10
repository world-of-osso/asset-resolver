# Encrypted content

The CASC resolver decrypts encrypted BLTE chunks with every locally available TACT key and extracts partially encrypted files when some keys are unknown. Implemented in [`src/casc_resolver.rs`](../../src/casc_resolver.rs) on top of `cascette-formats` `BlteFile::decompress_zeroing_missing_keys`.

## What it must do

- [x] Load TACT keys from `data/tactkeys/WoW.txt` (wowdev/TACTKeys) and from the installed build's keyring config: the `.build.info` row matching the selected product and its `.product.db` build key names `KeyRing`, read from `Data/config/<h[0..2]>/<h[2..4]>/<h>`.
- [x] Treat keyring IDs as key-name bytes in BLTE order (`key-4eb4869f95f23b53` is TACT key `533BF2959F86B44E`).
- [x] A product row with an empty `KeyRing` loads no keyring keys; a missing `.build.info` row for the selected build, or an unreadable/invalid keyring, is an error.
- [x] Zero-fill an encrypted chunk whose key is unknown at its declared decompressed size (CascLib `CASC_OVERCOME_ENCRYPTED`), warn with each key name, chunk index, offset and size, and return them in `ExtractedFile::missing_keys`.
- [x] Fail on chunk MD5 mismatch or malformed chunk headers instead of zero-filling.
- [x] Accept encrypted BLTE IV lengths of four or eight bytes. For Salsa20, preserve all IV bytes, zero-pad four-byte IVs to eight, and XOR the chunk index into the first four bytes. Reject other IV lengths and truncated headers.
- [ ] Decode root and encoding files strictly; a missing key there is an error.

## How it works

- DB2 readers skip zero-filled encrypted sections: wowdev DBCD `DBCD.IO/Readers/WDC5Reader.cs` treats a section with a nonzero `TactKeyLookup` and all-zero record data as absent.

## Implementation inventory

- `src/casc_resolver.rs` — key loading, partial BLTE decode, `ExtractedFile`, missing-key warning.
- `src/bin/casc_local.rs` — prints missing keys per extracted FDID.
- [`vendor/README.md`](../../vendor/README.md) — pinned formats patch, CascLib reference, provenance and retirement criteria.

## Tests asserting this spec

- `tests/blte_iv.rs` — eight-byte encrypted container, nonzero chunk-index XOR, four-byte IV zero-padding, invalid/truncated IVs; deterministic synthetic-key vectors.

- `src/casc_resolver.rs` — `selected_build_keyring_is_loaded_as_tact_keys`, `product_without_keyring_loads_no_keyring_keys`, `keyring_for_other_build_is_an_error`.
- cascette-rs `crates/cascette-formats/src/blte/mod.rs` — `missing_key_tests`.
- cascette-rs `crates/cascette-formats/src/config/keyring_config.rs` — `tact_keys_use_blte_byte_order_for_key_ids`.

## Known gaps (current cycle)

- [ ] Disk-cached extractions keep their zero-filled ranges after the missing keys become available; they are not re-extracted.

## Out of scope

- Keys published only in `DBCache.bin` hotfixes: not read.
