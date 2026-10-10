# Local DBCache TACT key import

`casc-local` reads `tactkeys/WoW.txt` under its configured data root. Import local hotfix keys reproducibly, before extraction:

```text
python3 scripts/import_dbcache_keys.py --cache <install>/_retail_/Cache/ADB/enUS/DBCache.bin --cache <install>/_classic_beta_/Cache/ADB/enUS/DBCache.bin --lookup-db2 <local-extraction>/1302851.db2 --store <data>/tactkeys/WoW.txt
```

The installation is read-only. Missing cache paths are reported. No downloads. Output contains names and unmatched record IDs only, never key bytes. Store updates are locked, atomic, additive, and reject conflicting existing names. An unchanged store is not rewritten.

Supports observed XFTH version 9 and uncompressed WDC5 TactKeyLookup layout 4983962C. Unsupported/truncated input fails explicitly. TactKey payload is 16 key bytes; TactKeyLookup payload is 8 name bytes, little-endian. Join by record ID, not adjacency. Base local TactKeyLookup supplies names absent from hotfix records. Cached push=-1 TactKey data survives subsequent invalidations, matching DBCD.

Sources: wowdev/DBCD `DBCD.IO/Readers/HTFXReader.cs`, `DBCD.IO/Common/HTFXStructs.cs`, `DBCD.IO/HotfixReader.cs`; wowdev/WoWDBDefs `definitions/TactKey.dbd`, `definitions/TactKeyLookup.dbd`, `manifest.json`. Table hashes DF2F53CF and AFC190D1; lookup FDID1302851. No real keys in source or tests.

Focused proof: `python3 -m unittest discover -s tests -p test_dbcache_keys.py -v`.
