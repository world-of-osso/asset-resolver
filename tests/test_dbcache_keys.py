import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts/import_dbcache_keys.py'
spec = importlib.util.spec_from_file_location('dbcache_keys', SCRIPT)
loader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loader)


def cache(*records):
    return b'XFTH' + struct.pack('<II', 9, 69933) + bytes(32) + b''.join(records)


def record(table, row, payload, status=1, push=5):
    return b'XFTH' + struct.pack('<IiiIII', 1, push, row, table, row, len(payload)) + bytes([status, 0, 0, 0]) + payload


class DBCacheKeysTests(unittest.TestCase):
    def test_joins_nonadjacent_lookup_and_key_by_id_little_endian(self):
        name = bytes.fromhex('8877665544332211')
        key = bytes(range(16))
        data = cache(record(0xAFC190D1, 7, name), record(0x12345678, 8, b'noise'), record(0xDF2F53CF, 7, key))
        keys, unmatched = loader.harvest(data, {})
        self.assertEqual(keys, {'1122334455667788': key.hex().upper()})
        self.assertEqual(unmatched, [])

    def test_joins_base_lookup_and_retains_cached_key_after_invalidation(self):
        key = bytes(range(16))
        data = cache(record(0xDF2F53CF, 7, key, push=-1), record(0xDF2F53CF, 7, b'', status=4))
        keys, unmatched = loader.harvest(data, {7: '1122334455667788'})
        self.assertEqual(keys, {'1122334455667788': key.hex().upper()})
        self.assertEqual(unmatched, [])

    def test_reports_unmatched_record_ids_without_inventing_names(self):
        keys, unmatched = loader.harvest(cache(record(0xDF2F53CF, 9, bytes(16))), {})
        self.assertEqual(keys, {})
        self.assertEqual(unmatched, [9])

    def test_rejects_truncated_record_and_wrong_payload_size(self):
        for data in [cache(record(0xDF2F53CF, 9, bytes(16)))[:-1], cache(record(0xDF2F53CF, 9, bytes(15)))]:
            with self.assertRaises(ValueError):
                loader.harvest(data, {})

    def test_atomic_merge_preserves_existing_and_rejects_conflicting_secret(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'WoW.txt'
            original = '# local source\n1122334455667788 ' + '00' * 16 + '\n'
            path.write_text(original)
            self.assertEqual(loader.merge_store(path, {'8877665544332211': '11' * 16}), ['8877665544332211'])
            self.assertTrue(path.read_text().startswith(original))
            before = path.read_bytes()
            with self.assertRaisesRegex(ValueError, 'conflicting key name 1122334455667788') as error:
                loader.merge_store(path, {'1122334455667788': 'DE' * 16})
            self.assertNotIn('DEDE', str(error.exception))
            self.assertEqual(path.read_bytes(), before)

    def test_reads_uncompressed_lookup_db2_with_noninline_ids(self):
        data = bytearray(288)
        data[:4] = b'WDC5'
        struct.pack_into('<6I', data, 136, 1, 1, 8, 0, 0xAFC190D1, 0x4983962C)
        struct.pack_into('<H', data, 172, 4)
        struct.pack_into('<I', data, 200, 1)
        struct.pack_into('<Q8I', data, 204, 0, 272, 1, 2, 280, 4, 0, 0, 0)
        struct.pack_into('<HHIIIII', data, 248, 0, 64, 0, 0, 0, 0, 0)
        struct.pack_into('<Q', data, 272, 0x1122334455667788)
        struct.pack_into('<I', data, 282, 7)
        self.assertEqual(loader.read_lookup(bytes(data)), {7: '1122334455667788'})


if __name__ == '__main__':
    unittest.main()
