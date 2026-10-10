"""Independent Salsa20/20 vectors; synthetic key only, no installed TACT keys."""
from pathlib import Path
import struct


def rotate(value, bits):
    return ((value << bits) | (value >> (32 - bits))) & 0xffffffff


def quarter_round(state, a, b, c, d):
    state[b] ^= rotate((state[a] + state[d]) & 0xffffffff, 7)
    state[c] ^= rotate((state[b] + state[a]) & 0xffffffff, 9)
    state[d] ^= rotate((state[c] + state[b]) & 0xffffffff, 13)
    state[a] ^= rotate((state[d] + state[c]) & 0xffffffff, 18)


def encrypt(iv, index):
    key = list(struct.unpack('<4I', bytes(range(16))))
    nonce = bytearray(iv.ljust(8, b'\0'))
    for i, byte in enumerate(index.to_bytes(4, 'little')):
        nonce[i] ^= byte
    state = [0x61707865, *key, 0x3120646e, *struct.unpack('<2I', nonce),
             0, 0, 0x79622d36, *key, 0x6b206574]
    plaintext = b'NDeterministic BLTE payload with a synthetic key. ' * 3
    ciphertext = bytearray()
    for offset in range(0, len(plaintext), 64):
        working = state.copy()
        for _ in range(10):
            for indices in [(0, 4, 8, 12), (5, 9, 13, 1), (10, 14, 2, 6),
                            (15, 3, 7, 11), (0, 1, 2, 3), (5, 6, 7, 4),
                            (10, 11, 8, 9), (15, 12, 13, 14)]:
                quarter_round(working, *indices)
        stream = struct.pack('<16I', *[(a + b) & 0xffffffff
                                     for a, b in zip(working, state)])
        ciphertext.extend(a ^ b for a, b in zip(plaintext[offset:offset + 64], stream))
        state[8] += 1
    return ciphertext


if __name__ == '__main__':
    for iv_size, index in [(8, 0), (8, 0x01020304), (4, 0x01020304)]:
        iv = bytes.fromhex('1122334455667788')[:iv_size]
        path = Path(__file__).with_name(f'blte-iv{iv_size}-block{index}.bin')
        path.write_bytes(encrypt(iv, index))
