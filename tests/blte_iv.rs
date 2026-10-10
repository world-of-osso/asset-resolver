#![cfg(feature = "casc")]

use std::io::Cursor;

use binrw::BinRead;
use cascette_crypto::{TactKey, TactKeyStore};
use cascette_formats::blte::{BlteFile, decrypt_chunk_with_keys};

const TEST_KEY_NAME: u64 = 0x0123_4567_89ab_cdef;
const TEST_KEY: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const IV: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
const BLOCK_INDEX: usize = 0x0102_0304;

fn test_keys() -> TactKeyStore {
    let mut keys = TactKeyStore::empty();
    keys.add(TactKey::new(TEST_KEY_NAME, TEST_KEY));
    keys
}

fn encrypted_chunk(iv: &[u8], ciphertext: &[u8]) -> Vec<u8> {
    let mut chunk = vec![8];
    chunk.extend_from_slice(&TEST_KEY_NAME.to_le_bytes());
    chunk.push(u8::try_from(iv.len()).unwrap());
    chunk.extend_from_slice(iv);
    chunk.push(b'S');
    chunk.extend_from_slice(ciphertext);
    chunk
}

fn expected_payload() -> Vec<u8> {
    let marked = b"NDeterministic BLTE payload with a synthetic key. ".repeat(3);
    marked[1..].to_vec()
}

// Fixed Salsa20/20 vectors generated independently with the synthetic key above,
// "expand 16-byte k", and block-index XOR on the first four nonce bytes.
// Nonzero upper IV bytes and >64 bytes of payload catch truncation/counter bugs.
#[test]
fn decodes_blte_container_with_eight_byte_iv() {
    let ciphertext = include_bytes!("fixtures/blte-iv8-block0.bin");
    let mut container = b"BLTE\0\0\0\0E".to_vec();
    container.extend_from_slice(&encrypted_chunk(&IV, ciphertext));
    let file = BlteFile::read_be(&mut Cursor::new(container)).unwrap();
    let decoded = file.decompress_zeroing_missing_keys(&test_keys()).unwrap();
    assert!(decoded.missing_keys.is_empty());
    assert_eq!(decoded.data, expected_payload());
}

#[test]
fn decodes_eight_byte_iv_with_block_index_xor() {
    let ciphertext = include_bytes!("fixtures/blte-iv8-block16909060.bin");
    let chunk = encrypted_chunk(&IV, ciphertext);
    assert_eq!(
        decrypt_chunk_with_keys(&chunk, &test_keys(), BLOCK_INDEX).unwrap(),
        expected_payload()
    );
}

#[test]
fn preserves_four_byte_iv_zero_padding() {
    let ciphertext = include_bytes!("fixtures/blte-iv4-block16909060.bin");
    let chunk = encrypted_chunk(&IV[..4], ciphertext);
    assert_eq!(
        decrypt_chunk_with_keys(&chunk, &test_keys(), BLOCK_INDEX).unwrap(),
        expected_payload()
    );
}

#[test]
fn rejects_invalid_and_truncated_ivs() {
    for size in [0, 3, 5, 7, 9] {
        let chunk = encrypted_chunk(&vec![0; size], &[0; 32]);
        let error = decrypt_chunk_with_keys(&chunk, &test_keys(), 0).unwrap_err();
        assert!(error.to_string().contains("Invalid IV size"));
    }
    let mut chunk = encrypted_chunk(&IV, &[0; 32]);
    chunk.truncate(17);
    let error = decrypt_chunk_with_keys(&chunk, &test_keys(), 0).unwrap_err();
    assert!(error.to_string().contains("too short for IV"));
}
