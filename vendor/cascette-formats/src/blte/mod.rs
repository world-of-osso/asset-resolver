//! BLTE (Block Table Encoded) format implementation
//!
//! BLTE is NGDP's container format for compressed and optionally encrypted content.
//! It provides block-based compression, encryption support, and efficient streaming
//! capabilities for game data delivery.
//!
//! # Features
//!
//! - Parser and builder for all BLTE modes
//! - Support for single and multi-chunk files
//! - Compression modes: None, `ZLib`, LZ4
//! - Encryption support: Salsa20, ARC4
//! - Round-trip validation

mod builder;
mod chunk;
mod compression;
mod encryption;
mod error;
mod header;
mod salsa20;

pub use builder::BlteBuilder;
pub use chunk::{ChunkData, CompressionMode};
pub use compression::{
    EncryptionSpec, compress_chunk, decompress_chunk, decrypt_chunk_with_keys,
    encrypt_chunk_with_key,
};
pub use encryption::{EncryptedHeader, EncryptionType};
pub use error::{BlteError, BlteResult};
pub use header::{BlteHeader, ChunkInfo, HeaderFlags};

use binrw::io::{Read, Seek, SeekFrom, Write};
use binrw::{BinRead, BinResult, BinWrite};
use cascette_crypto::TactKeyStore;

/// Complete BLTE file structure
#[derive(Debug, Clone)]
pub struct BlteFile {
    /// BLTE header
    pub header: BlteHeader,
    /// Chunk data
    pub chunks: Vec<ChunkData>,
}

impl BinRead for BlteFile {
    type Args<'a> = ();

    #[allow(clippy::cast_possible_truncation)]
    fn read_options<R: Read + Seek>(
        reader: &mut R,
        endian: binrw::Endian,
        _args: Self::Args<'_>,
    ) -> BinResult<Self> {
        // Read header
        let header = BlteHeader::read_options(reader, endian, ())?;

        // Read chunks based on header type
        let mut chunks = Vec::new();

        if header.is_single_chunk() {
            // For single chunk, we need to read the rest of the file
            // Get current position to calculate remaining size
            let start_pos = reader.stream_position()?;
            let end_pos = reader.seek(SeekFrom::End(0))?;
            reader.seek(SeekFrom::Start(start_pos))?;

            // Safe cast: file positions shouldn't exceed usize
            let chunk_size = (end_pos - start_pos) as usize;
            if chunk_size > 0 {
                let chunk = ChunkData::read_options(reader, endian, (chunk_size,))?;
                chunks.push(chunk);
            }
        } else {
            // Multi-chunk: read based on chunk info
            if let Some(ref extended) = header.extended {
                for info in &extended.chunk_infos {
                    let chunk =
                        ChunkData::read_options(reader, endian, (info.compressed_size as usize,))?;
                    chunks.push(chunk);
                }
            }
        }

        Ok(Self { header, chunks })
    }
}

impl BinWrite for BlteFile {
    type Args<'a> = ();

    fn write_options<W: Write + Seek>(
        &self,
        writer: &mut W,
        endian: binrw::Endian,
        _args: Self::Args<'_>,
    ) -> BinResult<()> {
        // Write header
        self.header.write_options(writer, endian, ())?;

        // Write chunks
        for chunk in &self.chunks {
            chunk.write_options(writer, endian, ())?;
        }

        Ok(())
    }
}

impl BlteFile {
    /// Create a new single-chunk BLTE file
    pub fn single_chunk(data: Vec<u8>, mode: CompressionMode) -> BlteResult<Self> {
        Ok(Self {
            header: BlteHeader::single_chunk(),
            chunks: vec![ChunkData::new(data, mode)?],
        })
    }

    /// Create a new multi-chunk BLTE file
    pub fn multi_chunk(chunks: Vec<ChunkData>) -> BlteResult<Self> {
        let header = BlteHeader::multi_chunk(&chunks)?;
        Ok(Self { header, chunks })
    }

    /// Decompress all chunks and return the complete data
    ///
    /// Performance: Pre-allocates the output buffer based on the total
    /// decompressed size from chunk headers or chunk metadata.
    pub fn decompress(&self) -> BlteResult<Vec<u8>> {
        // Performance: Pre-allocate with estimated total decompressed size
        let total_size = self.estimate_decompressed_size();
        let mut result = Vec::with_capacity(total_size);

        for (index, chunk) in self.chunks.iter().enumerate() {
            let decompressed = chunk.decompress(index)?;
            result.extend_from_slice(&decompressed);
        }
        Ok(result)
    }

    /// Decompress all chunks with decryption support
    ///
    /// Performance: Pre-allocates the output buffer based on the total
    /// decompressed size from chunk headers or chunk metadata.
    pub fn decompress_with_keys(&self, key_store: &TactKeyStore) -> BlteResult<Vec<u8>> {
        // Performance: Pre-allocate with estimated total decompressed size
        let total_size = self.estimate_decompressed_size();
        let mut result = Vec::with_capacity(total_size);

        for (index, chunk) in self.chunks.iter().enumerate() {
            result.extend_from_slice(&decode_chunk_with_keys(chunk, key_store, index)?);
        }
        Ok(result)
    }

    /// Decode all chunks, zero-filling encrypted chunks whose key is unknown
    ///
    /// Mirrors CascLib's `CASC_OVERCOME_ENCRYPTED` open flag: a chunk whose
    /// decryption key is missing becomes zeros of its declared decompressed
    /// size, and every such chunk is reported in [`PartialDecode::missing_keys`].
    /// Each chunk's MD5 is checked against the chunk table first, so corrupt
    /// data still fails with [`BlteError::ChecksumMismatch`] instead of being
    /// masked as a missing key. Single-chunk files have no declared size to
    /// zero-fill and still fail with [`BlteError::KeyNotFound`].
    pub fn decompress_zeroing_missing_keys(
        &self,
        key_store: &TactKeyStore,
    ) -> BlteResult<PartialDecode> {
        let Some(extended) = &self.header.extended else {
            return Ok(PartialDecode {
                data: self.decompress_with_keys(key_store)?,
                missing_keys: Vec::new(),
            });
        };

        let mut data = Vec::with_capacity(self.estimate_decompressed_size());
        let mut missing_keys = Vec::new();
        for (index, (chunk, info)) in self.chunks.iter().zip(&extended.chunk_infos).enumerate() {
            verify_chunk_checksum(chunk, info, index)?;
            match decode_chunk_with_keys(chunk, key_store, index) {
                Ok(decoded) => data.extend_from_slice(&decoded),
                Err(BlteError::KeyNotFound(key_name)) => {
                    let size = info.decompressed_size as usize;
                    missing_keys.push(MissingKeyChunk {
                        chunk_index: index,
                        key_name,
                        offset: data.len(),
                        size,
                    });
                    data.resize(data.len() + size, 0);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(PartialDecode { data, missing_keys })
    }

    /// Estimate total decompressed size from header or chunk metadata
    fn estimate_decompressed_size(&self) -> usize {
        // Try to get size from extended header first (most accurate)
        if let Some(ref extended) = self.header.extended {
            let total: u64 = extended
                .chunk_infos
                .iter()
                .map(|info| u64::from(info.decompressed_size))
                .sum();
            // Saturate to usize max to handle potential overflow gracefully
            return usize::try_from(total).unwrap_or(usize::MAX);
        }

        // Fall back to chunk-level estimates
        self.chunks.iter().map(|c| c.decompressed_size()).sum()
    }

    /// Compress data with automatic chunking
    pub fn compress(data: &[u8], chunk_size: usize, mode: CompressionMode) -> BlteResult<Self> {
        if data.len() <= chunk_size {
            // Single chunk
            Self::single_chunk(data.to_vec(), mode)
        } else {
            // Multi-chunk
            let mut chunks = Vec::new();
            let mut offset = 0;

            while offset < data.len() {
                let end = (offset + chunk_size).min(data.len());
                let chunk_data = data[offset..end].to_vec();
                chunks.push(ChunkData::new(chunk_data, mode)?);
                offset = end;
            }

            Self::multi_chunk(chunks)
        }
    }
}

/// Decoded BLTE content where some encrypted chunks had no known key
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialDecode {
    /// Decoded content; chunks listed in `missing_keys` are zero-filled
    pub data: Vec<u8>,
    /// Zero-filled chunks, in file order
    pub missing_keys: Vec<MissingKeyChunk>,
}

/// An encrypted chunk left zero-filled because its key is not in the key store
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingKeyChunk {
    /// Chunk index within the BLTE file
    pub chunk_index: usize,
    /// TACT key name as stored in the chunk header
    pub key_name: u64,
    /// Offset of the zero-filled range in the decoded content
    pub offset: usize,
    /// Declared decompressed size of the chunk (length of the zero-filled range)
    pub size: usize,
}

fn decode_chunk_with_keys(
    chunk: &ChunkData,
    key_store: &TactKeyStore,
    index: usize,
) -> BlteResult<Vec<u8>> {
    if chunk.mode == CompressionMode::Encrypted {
        decrypt_chunk_with_keys(&chunk.data, key_store, index)
    } else {
        chunk.decompress(index)
    }
}

fn verify_chunk_checksum(chunk: &ChunkData, info: &ChunkInfo, index: usize) -> BlteResult<()> {
    if chunk.verify_checksum(&info.checksum) {
        return Ok(());
    }
    let actual = cascette_crypto::md5::ContentKey::from_data(&chunk.compressed_data());
    Err(BlteError::ChecksumMismatch {
        expected: format!("chunk {index} {}", hex::encode(info.checksum)),
        actual: hex::encode(actual.as_bytes()),
    })
}

impl crate::CascFormat for BlteFile {
    fn parse(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        use std::io::Cursor;
        let mut cursor = Cursor::new(data);
        Self::read_options(&mut cursor, binrw::Endian::Big, ())
            .map_err(|e| Box::new(BlteError::BinRw(e)) as Box<dyn std::error::Error>)
    }

    fn build(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        use std::io::Cursor;
        let mut data = Vec::new();
        let mut cursor = Cursor::new(&mut data);
        self.write_options(&mut cursor, binrw::Endian::Big, ())
            .map_err(|e| Box::new(BlteError::BinRw(e)) as Box<dyn std::error::Error>)?;
        Ok(data)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::CascFormat;

    #[test]
    fn test_single_chunk_round_trip() {
        let data = b"Hello, BLTE!";
        let blte = BlteFile::single_chunk(data.to_vec(), CompressionMode::None)
            .expect("Test operation should succeed");

        // Use test utility for build-parse validation
        crate::test_utils::test_build_parse(&blte).expect("Build-parse should succeed");

        // Also verify the decompressed content is correct
        let built = blte.build().expect("Build should succeed");
        let parsed = BlteFile::parse(&built).expect("Parse should succeed");
        assert_eq!(parsed.decompress().expect("Operation should succeed"), data);
    }

    mod missing_key_tests {
        use super::*;
        use cascette_crypto::TactKey;

        const KNOWN_KEY_NAME: u64 = 0x533B_F295_9F86_B44E;
        const KNOWN_KEY: [u8; 16] = [
            0xC9, 0x31, 0x67, 0x39, 0x34, 0x8D, 0xCC, 0x03, 0x3A, 0xA8, 0x11, 0x2F, 0x9A, 0x3A,
            0xCF, 0x5D,
        ];
        const UNKNOWN_KEY_NAME: u64 = 0x583C_5B29_BF20_8655;
        const IV: [u8; 4] = [0x2A, 0x6E, 0x71, 0x03];

        fn encrypted_chunk(plain: &[u8], key_name: u64, key: &[u8; 16], index: usize) -> ChunkData {
            let mut inner = vec![CompressionMode::None.as_byte()];
            inner.extend_from_slice(plain);
            let spec = EncryptionSpec::salsa20(key_name, IV);
            let payload = encrypt_chunk_with_key(&inner, spec, key, index).expect("encrypt chunk");
            ChunkData::from_compressed(CompressionMode::Encrypted, payload, Some(plain.len()))
        }

        /// Serialized BLTE: plain, known-key, unknown-key, zlib chunks
        fn mixed_blte_bytes() -> Vec<u8> {
            let chunks = vec![
                ChunkData::new(b"WDC5 header".to_vec(), CompressionMode::None).expect("chunk"),
                encrypted_chunk(b"known section", KNOWN_KEY_NAME, &KNOWN_KEY, 1),
                encrypted_chunk(&[0xEE; 300], UNKNOWN_KEY_NAME, &[0x5A; 16], 2),
                ChunkData::new(b"trailer".to_vec(), CompressionMode::ZLib).expect("chunk"),
            ];
            BlteFile::multi_chunk(chunks)
                .expect("multi chunk")
                .build()
                .expect("build")
        }

        fn known_keys() -> TactKeyStore {
            let mut keys = TactKeyStore::empty();
            keys.add(TactKey::new(KNOWN_KEY_NAME, KNOWN_KEY));
            keys
        }

        #[test]
        fn unknown_key_chunk_is_zero_filled_at_declared_size_and_reported() {
            let blte = BlteFile::parse(&mixed_blte_bytes()).expect("parse");

            let decoded = blte
                .decompress_zeroing_missing_keys(&known_keys())
                .expect("decode");

            let mut expected = b"WDC5 header".to_vec();
            expected.extend_from_slice(b"known section");
            expected.extend_from_slice(&[0; 300]);
            expected.extend_from_slice(b"trailer");
            assert_eq!(decoded.data, expected);
            assert_eq!(
                decoded.missing_keys,
                vec![MissingKeyChunk {
                    chunk_index: 2,
                    key_name: UNKNOWN_KEY_NAME,
                    offset: 24,
                    size: 300,
                }]
            );
        }

        #[test]
        fn strict_decode_still_fails_on_unknown_key() {
            let blte = BlteFile::parse(&mixed_blte_bytes()).expect("parse");

            let result = blte.decompress_with_keys(&known_keys());

            assert!(matches!(
                result,
                Err(BlteError::KeyNotFound(UNKNOWN_KEY_NAME))
            ));
        }

        #[test]
        fn corrupt_unknown_key_chunk_fails_checksum_instead_of_zero_filling() {
            let mut bytes = mixed_blte_bytes();
            let header = BlteFile::parse(&bytes).expect("parse").header;
            let infos = &header.extended.as_ref().expect("multi chunk").chunk_infos;
            let chunk2_start = header.data_offset()
                + infos[..2]
                    .iter()
                    .map(|i| i.compressed_size as usize)
                    .sum::<usize>();
            bytes[chunk2_start + 100] ^= 0xFF; // inside the unknown-key chunk ciphertext
            let blte = BlteFile::parse(&bytes).expect("parse");

            let result = blte.decompress_zeroing_missing_keys(&known_keys());

            assert!(
                matches!(result, Err(BlteError::ChecksumMismatch { .. })),
                "{result:?}"
            );
        }

        #[test]
        fn malformed_encrypted_header_with_valid_checksum_still_errors() {
            let bad = ChunkData::from_compressed(
                CompressionMode::Encrypted,
                // key name size 4 instead of 8
                vec![4, 1, 2, 3, 4, 5, 6, 7, 8, 4, 1, 2, 3, 4, 0x53, 0, 0, 0],
                Some(3),
            );
            let plain = ChunkData::new(b"ok".to_vec(), CompressionMode::None).expect("chunk");
            let bytes = BlteFile::multi_chunk(vec![plain, bad])
                .expect("multi chunk")
                .build()
                .expect("build");
            let blte = BlteFile::parse(&bytes).expect("parse");

            let result = blte.decompress_zeroing_missing_keys(&known_keys());

            assert!(
                matches!(&result, Err(BlteError::CompressionError(msg)) if msg.contains("key name size")),
                "{result:?}"
            );
        }

        #[test]
        fn single_chunk_unknown_key_has_no_declared_size_and_errors() {
            let chunk = encrypted_chunk(b"secret", UNKNOWN_KEY_NAME, &[0x5A; 16], 0);
            let mut bytes = b"BLTE\0\0\0\0".to_vec();
            bytes.extend_from_slice(&chunk.compressed_data());
            let blte = BlteFile::parse(&bytes).expect("parse");

            let result = blte.decompress_zeroing_missing_keys(&known_keys());

            assert!(matches!(
                result,
                Err(BlteError::KeyNotFound(UNKNOWN_KEY_NAME))
            ));
        }
    }

    #[cfg(test)]
    mod proptest_tests {
        use super::*;
        use crate::blte::header::BLTE_MAGIC;
        use proptest::prelude::*;
        use proptest::test_runner::TestCaseError;

        /// Generate arbitrary compression modes (excluding deprecated Frame mode)
        fn compression_mode() -> impl Strategy<Value = CompressionMode> {
            prop_oneof![
                Just(CompressionMode::None),
                Just(CompressionMode::ZLib),
                Just(CompressionMode::LZ4),
            ]
        }

        /// Generate arbitrary data chunks (reasonable sizes for testing)
        fn data_chunk() -> impl Strategy<Value = Vec<u8>> {
            prop::collection::vec(any::<u8>(), 1..=10000)
        }

        /// Generate arbitrary header flags
        fn header_flags() -> impl Strategy<Value = HeaderFlags> {
            prop_oneof![Just(HeaderFlags::Standard), Just(HeaderFlags::Extended),]
        }

        proptest! {
                    /// Test that BLTE round-trip works for any valid data and compression mode
                    #[test]
                    fn blte_round_trip_always_works(
                        data in data_chunk(),
                        mode in compression_mode()
                    ) {
                        let blte = BlteFile::single_chunk(data.clone(), mode).map_err(|e| TestCaseError::fail(e.to_string()))?;
                        let serialized = blte.build().map_err(|e| TestCaseError::fail(e.to_string()))?;
                        let deserialized = BlteFile::parse(&serialized).map_err(|e| TestCaseError::fail(e.to_string()))?;
                        let decompressed = deserialized.decompress().map_err(|e| TestCaseError::fail(e.to_string()))?;

                        prop_assert_eq!(decompressed, data);
                    }

                    /// Test that invalid magic bytes are always rejected
                    fn invalid_magic_bytes_rejected(
                        magic in prop::array::uniform4(0u8..255).prop_filter("Not BLTE magic", |m| m != &BLTE_MAGIC)
                    ) {
                        let mut data = vec![0u8; 100];
                        data[0..4].copy_from_slice(&magic);

                        prop_assert!(BlteFile::parse(&data).is_err());
                    }

                    /// Test that multi-chunk files work correctly
                    #[test]
                    fn multi_chunk_round_trip(
                        chunks in prop::collection::vec(
                            (data_chunk(), compression_mode()),
                            1..10
                        ),
        _flags in header_flags()
                    ) {
                        // Create chunk data from test pairs
                        let chunk_data: Result<Vec<ChunkData>, BlteError> = chunks
                            .iter()
                            .map(|(data, mode)| ChunkData::new(data.clone(), *mode))
                            .collect();

                        let chunk_data = chunk_data.map_err(|e| TestCaseError::fail(e.to_string()))?;

                        // Create BLTE file with appropriate header
                        let blte = if chunk_data.len() == 1 {
                            BlteFile::single_chunk(chunks[0].0.clone(), chunks[0].1).map_err(|e| TestCaseError::fail(e.to_string()))?
                        } else {
                            BlteFile::multi_chunk(chunk_data).map_err(|e| TestCaseError::fail(e.to_string()))?
                        };

                        // Test round-trip
                        let serialized = blte.build().map_err(|e| TestCaseError::fail(e.to_string()))?;
                        let deserialized = BlteFile::parse(&serialized).map_err(|e| TestCaseError::fail(e.to_string()))?;
                        let decompressed = deserialized.decompress().map_err(|e| TestCaseError::fail(e.to_string()))?;

                        // Concatenate original data for comparison
                        let expected: Vec<u8> = chunks.into_iter()
                            .flat_map(|(data, _)| data)
                            .collect();

                        prop_assert_eq!(decompressed, expected);
                    }

                    /// Test that compression mode bytes are always valid
                    fn compression_mode_bytes_valid(mode in compression_mode()) {
                        let byte = mode.as_byte();
                        prop_assert!(CompressionMode::from_byte(byte).is_some());
                        prop_assert_eq!(CompressionMode::from_byte(byte).expect("Valid compression mode byte"), mode);
                    }

                    /// Test that invalid compression mode bytes are rejected
                    #[test]
                    fn invalid_compression_modes_rejected(
                        invalid_mode in any::<u8>().prop_filter(
                            "Not a valid compression mode",
                            |&b| !matches!(b, b'N' | b'Z' | b'4' | b'E' | b'F')
                        )
                    ) {
                        prop_assert!(CompressionMode::from_byte(invalid_mode).is_none());
                    }

                    /// Test that chunk count validation works correctly
                    fn chunk_count_validation(
                        chunk_count in 1u32..=0xFF_FFFF_u32
                    ) {
                        // Create dummy chunk data
                        let chunks: Vec<ChunkData> = (0..chunk_count.min(100)) // Limit to 100 for test performance
                            .map(|i| ChunkData::new(vec![i as u8; 10], CompressionMode::None))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|e| TestCaseError::fail(e.to_string()))?;

                        if chunks.len() <= 0xFF_FFFF {
                            let result = BlteHeader::multi_chunk(&chunks);
                            prop_assert!(result.is_ok());
                        } else {
                            // This branch won't execute due to our limit above, but shows the logic
                            let result = BlteHeader::multi_chunk(&chunks);
                            prop_assert!(result.is_err());
                        }
                    }

                    /// Test that header size calculations are consistent
                    fn header_size_calculations_consistent(
                        chunk_count in 1usize..=100,
        flags in header_flags()
                    ) {
                        let chunks: Vec<ChunkData> = (0..chunk_count)
                            .map(|i| ChunkData::new(vec![i as u8; 10], CompressionMode::None))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|e| TestCaseError::fail(e.to_string()))?;

                        let header = BlteHeader::multi_chunk(&chunks).map_err(|e| TestCaseError::fail(e.to_string()))?;

                        let expected_size = if header.is_single_chunk() {
                            8 // magic + header_size
                        } else {
                            // header_size includes 8-byte preamble + 4 (flags + count) + chunk_infos
                            12 + (chunk_count * flags.chunk_info_size())
                        };

                        if !header.is_single_chunk() {
                            prop_assert_eq!(header.header_size as usize, expected_size);
                        }
                        // For single-chunk: data starts at offset 8
                        // For multi-chunk: header_size already includes the preamble
                        prop_assert_eq!(header.data_offset(), if header.is_single_chunk() { 8 } else { header.header_size as usize });
                    }

                    /// Test that checksums are deterministic
                    #[test]
                    fn checksums_are_deterministic(
                        data in data_chunk(),
                        mode in compression_mode()
                    ) {
                        let chunk1 = ChunkData::new(data.clone(), mode).map_err(|e| TestCaseError::fail(e.to_string()))?;
                        let chunk2 = ChunkData::new(data, mode).map_err(|e| TestCaseError::fail(e.to_string()))?;

                        let info1 = ChunkInfo::from_chunk_data(&chunk1);
                        let info2 = ChunkInfo::from_chunk_data(&chunk2);

                        prop_assert_eq!(info1.checksum, info2.checksum);
                        prop_assert_eq!(info1.compressed_size, info2.compressed_size);
                        prop_assert_eq!(info1.decompressed_size, info2.decompressed_size);
                    }

                    /// Test that different data produces different checksums
                    fn different_data_different_checksums(
                        data1 in data_chunk(),
                        data2 in data_chunk(),
                        mode in compression_mode()
                    ) {
                        prop_assume!(data1 != data2); // Only test when data is actually different

                        let chunk1 = ChunkData::new(data1, mode).map_err(|e| TestCaseError::fail(e.to_string()))?;
                        let chunk2 = ChunkData::new(data2, mode).map_err(|e| TestCaseError::fail(e.to_string()))?;

                        let info1 = ChunkInfo::from_chunk_data(&chunk1);
                        let info2 = ChunkInfo::from_chunk_data(&chunk2);

                        // Different data should produce different checksums
                        prop_assert_ne!(info1.checksum, info2.checksum);
                    }

                    /// Test that automatic chunking produces consistent results
                    fn automatic_chunking_consistent(
                        data in prop::collection::vec(any::<u8>(), 1..=100_000),
                        chunk_size in 1000usize..=50000,
                        mode in compression_mode()
                    ) {
                        let blte = BlteFile::compress(&data, chunk_size, mode).map_err(|e| TestCaseError::fail(e.to_string()))?;
                        let decompressed = blte.decompress().map_err(|e| TestCaseError::fail(e.to_string()))?;

                        prop_assert_eq!(decompressed, data.clone());

                        // Verify chunk count is reasonable
                        let expected_chunks = data.len().div_ceil(chunk_size);
                        prop_assert_eq!(blte.chunks.len(), expected_chunks.max(1));
                    }

                    /// Test that header flags parsing is bijective
                    fn header_flags_bijective(flags in header_flags()) {
                        let byte = flags as u8;
                        let parsed = HeaderFlags::from_byte(byte);

                        prop_assert!(parsed.is_some());
                        prop_assert_eq!(parsed.expect("Valid header flags"), flags);
                    }
                }
    }
}
