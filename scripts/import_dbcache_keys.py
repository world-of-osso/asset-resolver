#!/usr/bin/env python3
"""Import local XFTH v9 TactKey hotfixes; print names only, never key material."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import struct
import tempfile

TACT_KEY = 0xDF2F53CF
TACT_LOOKUP = 0xAFC190D1
LOOKUP_LAYOUT = 0x4983962C


def take(data, offset, size):
    if offset < 0 or size < 0 or offset + size > len(data):
        raise ValueError(f'truncated input at offset {offset}, size {size}')
    return data[offset:offset + size]


def unpack(data, fmt, offset):
    return struct.unpack(fmt, take(data, offset, struct.calcsize(fmt)))


def read_records(data):
    if take(data, 0, 4) != b'XFTH' or unpack(data, '<I', 4)[0] != 9:
        raise ValueError('expected XFTH cache version 9')
    take(data, 0, 44)
    offset = 44
    records = []
    while offset < len(data):
        if take(data, offset, 4) != b'XFTH':
            raise ValueError(f'invalid record signature at offset {offset}')
        _, push, _, table, row, size = unpack(data, '<IiiIII', offset + 4)
        status = take(data, offset + 28, 4)[0]
        payload = take(data, offset + 32, size)
        if table in (TACT_KEY, TACT_LOOKUP):
            records.append((push, table, row, status, payload))
        offset += 32 + size
    return sorted(records, key=lambda record: record[0])


def harvest(data, base_lookup):
    records = read_records(data)
    names = dict(base_lookup)
    keys = {}
    # DBCD preserves cached (-1 push) TactKey data despite later invalidations.
    retain_cached = any(push == -1 and status == 1 and payload
                        for push, table, _, status, payload in records if table == TACT_KEY)
    for _, table, row, status, payload in records:
        target = keys if table == TACT_KEY else names
        if status != 1 or not payload:
            if table == TACT_LOOKUP or not retain_cached:
                target.pop(row, None)
            continue
        expected = 16 if table == TACT_KEY else 8
        if len(payload) != expected:
            raise ValueError(f'invalid TACT payload size for record {row}')
        target[row] = payload.hex().upper() if table == TACT_KEY else f'{int.from_bytes(payload, "little"):016X}'
    matched = {names[row]: key for row, key in keys.items() if row in names}
    return matched, sorted(keys.keys() - names.keys())


def read_lookup(data):
    if take(data, 0, 4) != b'WDC5':
        raise ValueError('expected WDC5 TactKeyLookup')
    _, fields, record_size, _, table, layout = unpack(data, '<6I', 136)
    flags = unpack(data, '<H', 172)[0]
    sections = unpack(data, '<I', 200)[0]
    if (fields, record_size, table, layout, flags) != (1, 8, TACT_LOOKUP, LOOKUP_LAYOUT, 4):
        raise ValueError('unsupported TactKeyLookup layout')
    field_storage = 204 + sections * 40 + fields * 4
    if unpack(data, '<HHIIIII', field_storage) != (0, 64, 0, 0, 0, 0, 0):
        raise ValueError('unsupported TactKeyLookup field storage')
    rows = {}
    for section in range(sections):
        key, start, count, strings, _, ids, relations, sparse, copies = unpack(data, '<Q8I', 204 + section * 40)
        if strings or relations or sparse or ids != count * 4:
            raise ValueError('unsupported TactKeyLookup section')
        payload = take(data, start, count * 8)
        if key and count and not any(payload):
            raise ValueError('unreadable TactKeyLookup section')
        id_start = start + count * 8
        for index in range(count):
            row = unpack(data, '<I', id_start + index * 4)[0]
            rows[row] = f'{unpack(payload, "<Q", index * 8)[0]:016X}'
        for new, source in struct.iter_unpack('<II', take(data, id_start + ids, copies * 8)):
            rows[new] = rows[source]
    return rows


def parse_store(text):
    result = {}
    for line in text.splitlines():
        fields = line.split()
        if not fields or fields[0].startswith('#'):
            continue
        if len(fields) < 2 or len(fields[0]) != 16 or len(fields[1]) != 32:
            raise ValueError('invalid existing key-store line')
        int(fields[0], 16)
        int(fields[1], 16)
        result[fields[0].upper()] = fields[1].upper()
    return result


def merge_store(path, additions):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.with_suffix(path.suffix + '.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        original = path.read_text() if path.exists() else ''
        existing = parse_store(original)
        for name, key in additions.items():
            if name in existing and existing[name] != key:
                raise ValueError(f'conflicting key name {name}')
        added = sorted(additions.keys() - existing.keys())
        if not added:
            return []
        suffix = ''.join(f'{name} {additions[name]}\n' for name in added)
        write_store_atomic(path, original.rstrip('\n') + '\n' + suffix)
        return added


def write_store_atomic(path, text):
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, path.stat().st_mode & 0o777 if path.exists() else 0o600)
        os.replace(temporary, path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache', type=Path, action='append', required=True)
    parser.add_argument('--lookup-db2', type=Path)
    parser.add_argument('--store', type=Path, required=True)
    args = parser.parse_args()
    lookup = read_lookup(args.lookup_db2.read_bytes()) if args.lookup_db2 else {}
    additions = {}
    sources = []
    for cache in args.cache:
        if not cache.exists():
            sources.append({'path': str(cache), 'status': 'absent'})
            continue
        keys, unmatched = harvest(cache.read_bytes(), lookup)
        for name, key in keys.items():
            if name in additions and additions[name] != key:
                raise ValueError(f'conflicting key name {name}')
            additions[name] = key
        sources.append({'path': str(cache), 'names': sorted(keys), 'unmatched_ids': unmatched})
    added = merge_store(args.store, additions)
    print(json.dumps({'sources': sources, 'added_names': added, 'known_names': sorted(additions)}, indent=2))


if __name__ == '__main__':
    main()
