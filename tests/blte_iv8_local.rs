#![cfg(feature = "casc")]

use std::io::Cursor;
use std::path::PathBuf;

use binrw::BinRead;
use cascette_client_storage::index::IndexManager;
use cascette_client_storage::storage::ArchiveManager;
use cascette_crypto::{ContentKey, EncodingKey, TactKeyStore};
use cascette_formats::blte::{BlteFile, CompressionMode};

const FDID: u32 = 3_182_457;
const ENCODING_KEY: &str = "910058c8537e36682eb1f2a24c74178e";
const CONTENT_KEY: &str = "8e085d4946b784ca77ad1be1f24e52c9";
const DECODED_SIZE: usize = 163_908;
const LOCAL_ARCHIVE_HEADER_SIZE: usize = 30;

// Retail 12.1.0.69933: one of the closure-extract IV8 failures recovered by
// blteiv. Read authentic bytes from local CASC, never from an extracted cache.
// Keys stay outside the repository. Explicit invocation fails if inputs vanish.
#[test]
#[ignore = "requires local WoW archives and external TACT keys; see docs/specs/encrypted-content.md"]
fn decodes_recovered_fdid_3182457_with_eight_byte_iv() {
    let archive_dir = PathBuf::from(
        std::env::var_os("CASC_IV8_INSTALL").expect("set CASC_IV8_INSTALL to the WoW install"),
    )
    .join("Data/data");
    let keys_path = std::env::var_os("CASC_IV8_KEYS").expect("set CASC_IV8_KEYS to WoW.txt");
    let keys_text = std::fs::read_to_string(keys_path).expect("read external TACT keys");
    let mut keys = TactKeyStore::empty();
    assert!(
        keys.load_from_txt(&keys_text) > 0,
        "no external keys loaded"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut indices = IndexManager::new(&archive_dir);
    let mut archives = ArchiveManager::new(&archive_dir);
    runtime.block_on(indices.load_all()).unwrap();
    runtime.block_on(archives.open_all()).unwrap();
    let encoding_key = EncodingKey::from_hex(ENCODING_KEY).unwrap();
    let entry = indices.lookup(&encoding_key).expect("local IV8 entry");
    let raw = archives
        .read_raw(entry.archive_id(), entry.archive_offset(), entry.size)
        .unwrap();
    let container = &raw[LOCAL_ARCHIVE_HEADER_SIZE..];
    assert_eq!(&container[..4], b"BLTE");
    let file = BlteFile::read_be(&mut Cursor::new(container)).unwrap();
    assert!(file.chunks.iter().any(|chunk| {
        chunk.mode == CompressionMode::Encrypted
            && chunk.data.first() == Some(&8)
            && chunk.data.get(9) == Some(&8)
    }));

    // Match the resolver's key-aware decode, including missing-key handling.
    let decoded = file.decompress_zeroing_missing_keys(&keys).unwrap();
    assert!(decoded.missing_keys.is_empty());
    assert_eq!(decoded.data.len(), DECODED_SIZE);
    assert_eq!(
        ContentKey::from_data(&decoded.data).to_string(),
        CONTENT_KEY
    );
    println!("FDID {FDID}: IV8 decoded {DECODED_SIZE} bytes, CKey {CONTENT_KEY}");
}
