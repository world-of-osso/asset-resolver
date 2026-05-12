use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, UNIX_EPOCH};

use cascette_crypto::{ContentKey, EncodingKey};
use rusqlite::{Connection, ErrorCode, OpenFlags};

const SCHEMA_VERSION: i64 = 1;
type FdidToContentKeyMap = HashMap<u32, ContentKey>;
type ContentToEncodingKeyMap = HashMap<ContentKey, EncodingKey>;

pub struct CascResolutionCache {
    conn: Mutex<Connection>,
}

impl CascResolutionCache {
    /// Open an existing resolution cache in read-only mode.
    ///
    /// Returns an error if the cache doesn't exist or is stale relative to
    /// root.bin / encoding.bin. Run [`build_resolution_cache`] first (via `casc_refresh`).
    pub fn open(casc_dir: &Path) -> Result<Self, String> {
        let cache_path = casc_dir.join("resolution.sqlite");
        let root_path = casc_dir.join("root.bin");
        let enc_path = casc_dir.join("encoding.bin");

        if !cache_path.exists() {
            return Err(format!(
                "{} not found (run `casc_refresh` to build it)",
                cache_path.display()
            ));
        }

        let root_mtime = file_mtime(&root_path)?;
        let enc_mtime = file_mtime(&enc_path)?;

        if !cache_is_fresh(&cache_path, root_mtime, enc_mtime)? {
            return Err(format!(
                "{} is stale (run `casc_refresh` to rebuild it)",
                cache_path.display()
            ));
        }

        let conn = Connection::open_with_flags(
            &cache_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("open {}: {e}", cache_path.display()))?;

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM resolution", [], |row| row.get(0))
            .map_err(|e| format!("count resolution entries: {e}"))?;
        eprintln!("CASC resolution cache: {n} entries");

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Query fdid → (content_key bytes, encoding_key bytes).
    pub fn resolve_fdid(&self, fdid: u32) -> Option<([u8; 16], [u8; 16])> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT content_key, encoding_key FROM resolution WHERE fdid = ?1",
                [fdid],
                |row| {
                    let ck: Vec<u8> = row.get(0)?;
                    let ek: Vec<u8> = row.get(1)?;
                    Ok((ck, ek))
                },
            )
            .ok()
            .and_then(|(ck, ek)| {
                let ck: [u8; 16] = ck.try_into().ok()?;
                let ek: [u8; 16] = ek.try_into().ok()?;
                Some((ck, ek))
            })
    }
}

pub fn resolution_cache_is_fresh(casc_dir: &Path) -> Result<bool, String> {
    let (cache_path, root_path, enc_path) = resolution_paths(casc_dir);
    if !cache_path.exists() {
        return Ok(false);
    }

    // If a previous bootstrap was killed mid-write, SQLite leaves a rollback
    // journal alongside the .sqlite file. Read-only opens then fail with
    // "attempt to write a readonly database" because SQLite needs to recover
    // the journal. Wipe the half-written cache so the caller rebuilds from
    // CASC instead of erroring out forever.
    let journal_path = cache_path.with_extension("sqlite-journal");
    let wal_path = cache_path.with_extension("sqlite-wal");
    if journal_path.exists() || wal_path.exists() {
        let _ = std::fs::remove_file(&cache_path);
        let _ = std::fs::remove_file(&journal_path);
        let _ = std::fs::remove_file(&wal_path);
        return Ok(false);
    }

    let root_mtime = match file_mtime(&root_path) {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    let enc_mtime = match file_mtime(&enc_path) {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    cache_is_fresh(&cache_path, root_mtime, enc_mtime)
}

/// Build (or rebuild) the resolution SQLite cache from root.bin + encoding.bin.
///
/// Called by `casc_refresh` after writing the binary files.
pub fn build_resolution_cache(casc_dir: &Path) -> Result<(), String> {
    let (cache_path, root_path, enc_path) = resolution_paths(casc_dir);
    let started_at = Instant::now();
    let (fdid_to_ck, ck_to_ek) = build_resolution_maps(&root_path, &enc_path)?;
    let conn = open_resolution_cache(&cache_path)?;
    init_resolution_schema(&conn)?;
    let inserted_rows = insert_resolution_rows(&conn, &fdid_to_ck, &ck_to_ek)?;
    // Capture mtimes AFTER the parse/insert. Some upstream code (notably
    // cascette's lazy index initialization) may rewrite root.bin/encoding.bin
    // partway through the build, leaving the captured-at-start mtimes stale
    // by the time the cache is reopened. Reading them post-parse closes the
    // window and prevents a freshness check from immediately invalidating
    // the cache we just built.
    let root_mtime = file_mtime(&root_path)?;
    let enc_mtime = file_mtime(&enc_path)?;
    insert_resolution_metadata(&conn, root_mtime, enc_mtime)?;
    commit_resolution_cache(&conn)?;
    log_resolution_cache_build(inserted_rows, started_at);
    Ok(())
}

fn resolution_paths(casc_dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        casc_dir.join("resolution.sqlite"),
        casc_dir.join("root.bin"),
        casc_dir.join("encoding.bin"),
    )
}

fn build_resolution_maps(
    root_path: &Path,
    enc_path: &Path,
) -> Result<(FdidToContentKeyMap, ContentToEncodingKeyMap), String> {
    let root_data = read_cache_file(root_path)?;
    let enc_data = read_cache_file(enc_path)?;
    let root = cascette_formats::root::RootFile::parse(&root_data)
        .map_err(|e| format!("parse root.bin: {e}"))?;
    let encoding = cascette_formats::encoding::EncodingFile::parse(&enc_data)
        .map_err(|e| format!("parse encoding.bin: {e}"))?;
    let fdid_to_ck = collect_fdid_to_content_keys(&root);
    let ck_to_ek = collect_content_to_encoding_keys(&encoding);
    Ok((fdid_to_ck, ck_to_ek))
}

fn read_cache_file(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn collect_fdid_to_content_keys(root: &cascette_formats::root::RootFile) -> FdidToContentKeyMap {
    // last-write-wins, matches ContentResolver behavior
    let mut fdid_to_ck = HashMap::new();
    for block in &root.blocks {
        for record in &block.records {
            fdid_to_ck.insert(record.file_data_id.get(), record.content_key);
        }
    }
    fdid_to_ck
}

fn collect_content_to_encoding_keys(
    encoding: &cascette_formats::encoding::EncodingFile,
) -> ContentToEncodingKeyMap {
    // first encoding key per content
    let mut ck_to_ek = HashMap::new();
    for page in &encoding.ckey_pages {
        for entry in &page.entries {
            if let Some(encoding_key) = entry.encoding_keys.first() {
                ck_to_ek.insert(entry.content_key, *encoding_key);
            }
        }
    }
    ck_to_ek
}

fn open_resolution_cache(cache_path: &Path) -> Result<Connection, String> {
    Connection::open(cache_path).map_err(|e| format!("open {}: {e}", cache_path.display()))
}

fn init_resolution_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "BEGIN;
         DROP TABLE IF EXISTS metadata;
         DROP TABLE IF EXISTS resolution;
         CREATE TABLE metadata (
             root_mtime INTEGER NOT NULL,
             enc_mtime INTEGER NOT NULL,
             schema_version INTEGER NOT NULL
         );
         CREATE TABLE resolution (
             fdid INTEGER PRIMARY KEY,
             content_key BLOB NOT NULL,
             encoding_key BLOB NOT NULL
         );",
    )
    .map_err(|e| format!("init resolution cache schema: {e}"))
}

fn insert_resolution_rows(
    conn: &Connection,
    fdid_to_ck: &FdidToContentKeyMap,
    ck_to_ek: &ContentToEncodingKeyMap,
) -> Result<usize, String> {
    let mut insert = conn
        .prepare("INSERT INTO resolution (fdid, content_key, encoding_key) VALUES (?1, ?2, ?3)")
        .map_err(|e| format!("prepare resolution insert: {e}"))?;
    let mut inserted_rows = 0usize;

    for (&fdid, content_key) in fdid_to_ck {
        let Some(encoding_key) = ck_to_ek.get(content_key) else {
            continue;
        };
        insert
            .execute((
                fdid,
                content_key.as_bytes().as_ref(),
                encoding_key.as_bytes().as_ref(),
            ))
            .map_err(|e| format!("insert resolution entry {fdid}: {e}"))?;
        inserted_rows += 1;
    }

    drop(insert);
    Ok(inserted_rows)
}

fn insert_resolution_metadata(
    conn: &Connection,
    root_mtime: i64,
    enc_mtime: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO metadata (root_mtime, enc_mtime, schema_version) VALUES (?1, ?2, ?3)",
        (root_mtime, enc_mtime, SCHEMA_VERSION),
    )
    .map_err(|e| format!("insert resolution metadata: {e}"))?;
    Ok(())
}

fn commit_resolution_cache(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("COMMIT;")
        .map_err(|e| format!("commit resolution cache: {e}"))
}

fn log_resolution_cache_build(inserted_rows: usize, started_at: Instant) {
    let elapsed = started_at.elapsed().as_secs_f64();
    eprintln!("Built CASC resolution cache ({inserted_rows} entries) in {elapsed:.1}s");
}

fn file_mtime(path: &Path) -> Result<i64, String> {
    let modified = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .modified()
        .map_err(|e| format!("mtime {}: {e}", path.display()))?;
    Ok(modified
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("mtime epoch {}: {e}", path.display()))?
        .as_secs() as i64)
}

fn cache_is_fresh(cache_path: &Path, root_mtime: i64, enc_mtime: i64) -> Result<bool, String> {
    let conn = Connection::open_with_flags(
        cache_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open {}: {e}", cache_path.display()))?;

    let mut stmt = match conn
        .prepare("SELECT root_mtime, enc_mtime, schema_version FROM metadata LIMIT 1")
    {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(ref msg))) if msg.contains("no such table") => {
            return Ok(false);
        }
        Err(rusqlite::Error::SqliteFailure(ref err, _)) if is_corrupt_cache_error(err.code) => {
            return Ok(false);
        }
        Err(e) => return Err(format!("prepare metadata query: {e}")),
    };

    let row = stmt.query_row([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });

    match row {
        Ok((stored_root, stored_enc, stored_version)) => Ok(stored_root == root_mtime
            && stored_enc == enc_mtime
            && stored_version == SCHEMA_VERSION),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(rusqlite::Error::SqliteFailure(ref err, _)) if is_corrupt_cache_error(err.code) => {
            Ok(false)
        }
        Err(e) => Err(format!("query metadata: {e}")),
    }
}

fn is_corrupt_cache_error(code: ErrorCode) -> bool {
    matches!(code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn unique_temp_casc_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "asset-resolver-casc-cache-test-{}-{unique}",
            std::process::id()
        ))
    }

    fn create_temp_casc_dir() -> PathBuf {
        let casc_dir = unique_temp_casc_dir();
        std::fs::create_dir_all(&casc_dir).expect("create temp casc dir");
        std::fs::write(casc_dir.join("root.bin"), b"root").expect("write root.bin");
        std::fs::write(casc_dir.join("encoding.bin"), b"encoding").expect("write encoding.bin");
        casc_dir
    }

    #[test]
    fn open_and_lookup() {
        let casc_dir = create_temp_casc_dir();
        let cache_path = casc_dir.join("resolution.sqlite");
        let conn = Connection::open(&cache_path).expect("create cache database");
        let content_key = [1u8; 16];
        let encoding_key = [2u8; 16];

        init_resolution_schema(&conn).expect("create resolution schema");
        conn.execute(
            "INSERT INTO resolution (fdid, content_key, encoding_key) VALUES (?1, ?2, ?3)",
            (120191u32, content_key.as_slice(), encoding_key.as_slice()),
        )
        .expect("insert resolution row");
        insert_resolution_metadata(
            &conn,
            file_mtime(&casc_dir.join("root.bin")).expect("root mtime"),
            file_mtime(&casc_dir.join("encoding.bin")).expect("encoding mtime"),
        )
        .expect("insert metadata row");
        commit_resolution_cache(&conn).expect("commit cache database");
        drop(conn);

        let cache = CascResolutionCache::open(&casc_dir).expect("failed to open cache");
        let result = cache.resolve_fdid(120191);

        std::fs::remove_dir_all(&casc_dir).expect("remove temp casc dir");
        assert!(result.is_some(), "expected fdid 120191 to resolve");
        let (ck, ek) = result.unwrap();
        assert_eq!(ck, content_key);
        assert_eq!(ek, encoding_key);
    }

    #[test]
    fn resolution_cache_is_fresh_errors_when_cache_cannot_be_opened() {
        let casc_dir = create_temp_casc_dir();
        std::fs::create_dir(casc_dir.join("resolution.sqlite"))
            .expect("create directory at cache path");

        let result = resolution_cache_is_fresh(&casc_dir);

        std::fs::remove_dir_all(&casc_dir).expect("remove temp casc dir");
        assert!(result.is_err(), "expected open failure, got {result:?}");
    }

    #[test]
    fn resolution_cache_is_fresh_returns_false_without_metadata_table() {
        let casc_dir = create_temp_casc_dir();
        Connection::open(casc_dir.join("resolution.sqlite")).expect("create empty cache database");

        let result = resolution_cache_is_fresh(&casc_dir);

        std::fs::remove_dir_all(&casc_dir).expect("remove temp casc dir");
        assert_eq!(result, Ok(false));
    }

    #[test]
    fn resolution_cache_is_fresh_returns_false_without_metadata_row() {
        let casc_dir = create_temp_casc_dir();
        let conn =
            Connection::open(casc_dir.join("resolution.sqlite")).expect("create cache database");
        init_resolution_schema(&conn).expect("create resolution schema");
        commit_resolution_cache(&conn).expect("commit cache database");

        let result = resolution_cache_is_fresh(&casc_dir);

        std::fs::remove_dir_all(&casc_dir).expect("remove temp casc dir");
        assert_eq!(result, Ok(false));
    }
}
