# cascette-formats BLTE IV patch

Source: Osso/cascette-rs commit `027a1ac34c81b4ec23a0bfa36ea298f6642abe19`,
`crates/cascette-formats`; MIT/Apache-2.0 licenses retained in the crate.
The manifest expands inherited workspace metadata/dependencies for standalone use.
The root Cargo patch applies to both asset-resolver and client-storage.

Root cause: `blte/compression.rs::decrypt_chunk_with_keys` accepted only four
IV bytes. The downstream `cascette-crypto::Salsa20Cipher::new` also rejects
anything except four. Merely relaxing the header check would still fail.

Local changes: accept four or eight bytes, consume the complete IV before the
cipher-type byte, and decrypt Salsa20 through RustCrypto's existing 0.10 core.
BLTE uses the 128-bit-key tau constants, a repeated key, a zero-padded eight-byte
nonce, and frame-index XOR of the first four bytes. Eight-byte IVs retain their
upper bytes. Four-byte encryption builders and ARC4 behavior are unchanged.
No key store or real TACT key material is vendored.

Reference: [CascLib `CascDecrypt`](https://github.com/ladislav-zezula/CascLib/blob/master/src/CascDecrypt.cpp)
explicitly checks `pbInBuffer[0] != 4 && pbInBuffer[0] != 8`, zeroes `Vector[8]`,
copies `IVSize` bytes, XORs the frame index into its first four bytes, and passes
all eight bytes to Salsa20. Its header accepts `S`/`A`; ARC4 support is not added
by this patch.

Regression tests: `cargo test --test blte_iv`. Fixtures use only the synthetic
key `00..0f`, nonzero upper IV bytes, and a multi-keystream-block payload.
`tests/fixtures/generate_blte_vectors.py` deterministically rebuilds them using
an independent Salsa20/20 implementation (not the production decryption code).

Retirement: remove this vendor tree and Cargo patch once the pinned upstream
formats/crypto dependencies support full eight-byte BLTE IVs and pass these
same regression tests.
