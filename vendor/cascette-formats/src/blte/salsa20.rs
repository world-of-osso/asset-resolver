//! BLTE's Salsa20/20 variant: 128-bit key and a 4- or 8-byte IV.

use salsa20::cipher::{StreamCipher, consts::U10};
use salsa20::{Salsa20, SalsaCore};

use super::error::{BlteError, BlteResult};

pub(super) fn decrypt_salsa20(
    data: &[u8],
    key: &[u8; 16],
    iv: &[u8],
    block_index: usize,
) -> BlteResult<Vec<u8>> {
    let state = initialize_salsa20_state(key, iv, block_index);
    let mut cipher = Salsa20::from_core(SalsaCore::<U10>::from_raw_state(state));
    let mut output = data.to_vec();
    cipher.try_apply_keystream(&mut output).map_err(|error| {
        BlteError::CompressionError(format!("Salsa20 keystream exhausted: {error}"))
    })?;
    Ok(output)
}

fn initialize_salsa20_state(key: &[u8; 16], iv: &[u8], block_index: usize) -> [u32; 16] {
    // IV length is validated by the encrypted-chunk parser. Preserve all eight
    // bytes; four-byte IVs are zero-padded, as in CascLib's CascDecrypt.
    let mut nonce = [0u8; 8];
    nonce[..iv.len()].copy_from_slice(iv);
    for (byte, index_byte) in nonce[..4].iter_mut().zip(block_index.to_le_bytes()) {
        *byte ^= index_byte;
    }

    // RustCrypto's default constructor uses 256-bit keys. BLTE requires the
    // "expand 16-byte k" constants and the same 128-bit key in both key slots.
    let mut state = [0u32; 16];
    state[0] = 0x6170_7865;
    state[5] = 0x3120_646e;
    state[10] = 0x7962_2d36;
    state[15] = 0x6b20_6574;
    for (index, bytes) in key.chunks_exact(4).enumerate() {
        let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        state[index + 1] = word;
        state[index + 11] = word;
    }
    for (index, bytes) in nonce.chunks_exact(4).enumerate() {
        state[index + 6] = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    state
}
