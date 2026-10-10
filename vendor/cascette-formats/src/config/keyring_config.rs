//! Keyring Config file format implementation
//!
//! Keyring Config files contain encryption keys for decrypting protected CASC content.
//! Each entry maps an 8-byte key ID to a 16-byte encryption key. Agent.exe uses these
//! keys via its `tact::KeyGetter::LoadKeyring` function for Salsa20 decryption of BLTE
//! encrypted blocks.
//!
//! Keyring config hashes are referenced in the Ribbit versions response `KeyRing` column,
//! not in build configs. The config is fetched from CDN using the standard config path.

use std::io::{BufRead, BufReader, Read, Write};

use cascette_crypto::TactKey;

use super::{is_valid_md5_hex, parse_line};

/// Keyring Configuration containing encryption key entries
///
/// Format: `key-{KEY_ID_HEX} = {KEY_VALUE_HEX}` per line, where KEY_ID is 16 hex
/// chars (8 bytes) and KEY_VALUE is 32 hex chars (16 bytes).
#[derive(Debug, Clone)]
pub struct KeyringConfig {
    /// Ordered list of keyring entries
    entries: Vec<KeyringEntry>,
}

/// A single encryption key entry from the keyring
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyringEntry {
    /// 8-byte key identifier as 16 hex characters (lowercase)
    pub key_id: String,
    /// 16-byte encryption key as 32 hex characters (lowercase)
    pub key_value: String,
}

/// Key ID prefix in config files
const KEY_PREFIX: &str = "key-";

impl KeyringConfig {
    /// Create a new empty `KeyringConfig`
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Parse `KeyringConfig` from a reader
    pub fn parse<R: Read>(reader: R) -> Result<Self, Box<dyn std::error::Error>> {
        let mut entries = Vec::new();
        let reader = BufReader::new(reader);

        for line in reader.lines() {
            let line = line?;
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = parse_line(line) {
                // Keys must start with "key-"
                let Some(key_id) = key.strip_prefix(KEY_PREFIX) else {
                    continue;
                };

                // Normalize to lowercase for consistent lookups
                let key_id = key_id.to_ascii_lowercase();
                let key_value = value.to_ascii_lowercase();

                entries.push(KeyringEntry { key_id, key_value });
            }
        }

        Ok(Self { entries })
    }

    /// Build the config file content
    pub fn build(&self) -> Vec<u8> {
        let mut output = Vec::new();

        for entry in &self.entries {
            let _ = writeln!(
                output,
                "{}{} = {}",
                KEY_PREFIX, entry.key_id, entry.key_value
            );
        }

        output
    }

    /// Validate the keyring configuration
    pub fn validate(&self) -> Result<(), ValidationError> {
        for (i, entry) in self.entries.iter().enumerate() {
            // Key ID must be 16 hex characters (8 bytes)
            if entry.key_id.len() != 16 {
                return Err(ValidationError::IdLength {
                    index: i,
                    actual: entry.key_id.len(),
                });
            }
            if !entry.key_id.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(ValidationError::IdFormat {
                    index: i,
                    value: entry.key_id.clone(),
                });
            }

            // Key value must be a valid 32-char hex string (16 bytes)
            if !is_valid_md5_hex(&entry.key_value) {
                return Err(ValidationError::KeyValue {
                    index: i,
                    key_id: entry.key_id.clone(),
                });
            }
        }

        Ok(())
    }

    /// Get all entries
    pub fn entries(&self) -> &[KeyringEntry] {
        &self.entries
    }

    /// Number of key entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the keyring is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a key by its 16-char hex key ID
    ///
    /// Returns the 32-char hex key value if found. The lookup is case-insensitive.
    pub fn get_key(&self, key_id: &str) -> Option<&str> {
        let key_id_lower = key_id.to_ascii_lowercase();
        self.entries
            .iter()
            .find(|e| e.key_id == key_id_lower)
            .map(|e| e.key_value.as_str())
    }

    /// Look up a key by its numeric u64 key ID
    ///
    /// Returns the 32-char hex key value if found.
    pub fn get_key_by_id(&self, id: u64) -> Option<&str> {
        let hex_id = format!("{id:016x}");
        self.get_key(&hex_id)
    }

    /// Convert all entries to TACT keys for BLTE decryption
    ///
    /// A keyring ID is the key name's bytes in BLTE chunk header order
    /// (little-endian), the same order encoding-file ESpec strings use. So
    /// `key-4eb4869f95f23b53` is TACT key `533BF2959F86B44E` in the
    /// wowdev/TACTKeys list, which carries the same key value.
    pub fn tact_keys(&self) -> Result<Vec<TactKey>, ValidationError> {
        self.validate()?;
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let id =
                    decode_hex::<8>(&entry.key_id).ok_or_else(|| ValidationError::IdFormat {
                        index,
                        value: entry.key_id.clone(),
                    })?;
                let key = decode_hex::<16>(&entry.key_value).ok_or_else(|| {
                    ValidationError::KeyValue {
                        index,
                        key_id: entry.key_id.clone(),
                    }
                })?;
                Ok(TactKey::new(u64::from_le_bytes(id), key))
            })
            .collect()
    }

    /// Add a key entry
    ///
    /// Both key_id and key_value are normalized to lowercase.
    pub fn add_entry(&mut self, key_id: impl Into<String>, key_value: impl Into<String>) {
        self.entries.push(KeyringEntry {
            key_id: key_id.into().to_ascii_lowercase(),
            key_value: key_value.into().to_ascii_lowercase(),
        });
    }
}

impl Default for KeyringConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Keyring config validation errors
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    /// Key ID has wrong length (expected 16 hex chars)
    #[error("entry {index}: key ID must be 16 hex chars, got {actual}")]
    IdLength { index: usize, actual: usize },
    /// Key ID contains non-hex characters
    #[error("entry {index}: key ID is not valid hex: {value}")]
    IdFormat { index: usize, value: String },
    /// Key value is not a valid 32-char hex string
    #[error("entry {index} (key {key_id}): key value must be 32 hex chars")]
    KeyValue { index: usize, key_id: String },
}

fn decode_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    hex::decode(value).ok()?.try_into().ok()
}

impl crate::CascFormat for KeyringConfig {
    fn parse(data: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        Self::parse(data)
    }

    fn build(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        Ok(self.build())
    }
}

