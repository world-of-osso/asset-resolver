use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OpenFlags};

const SCHEMA_VERSION: i64 = 1;

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

/// Build (or rebuild) the resolution SQLite cache from root.bin + encoding.bin.
///
/// Called by `casc_refresh` after writing the binary files.
pub fn build_resolution_cache(casc_dir: &Path) -> Result<(), String> {
    let cache_path = casc_dir.join("resolution.sqlite");
    let root_path = casc_dir.join("root.bin");
    let enc_path = casc_dir.join("encoding.bin");

    let root_mtime = file_mtime(&root_path)?;
    let enc_mtime = file_mtime(&enc_path)?;

    let t0 = std::time::Instant::now();

    let root_data = std::fs::read(&root_path)
        .map_err(|e| format!("{}: {e}", root_path.display()))?;
    let enc_data = std::fs::read(&enc_path)
        .map_err(|e| format!("{}: {e}", enc_path.display()))?;

    let root = cascette_formats::root::RootFile::parse(&root_data)
        .map_err(|e| format!("parse root.bin: {e}"))?;
    let encoding = cascette_formats::encoding::EncodingFile::parse(&enc_data)
        .map_err(|e| format!("parse encoding.bin: {e}"))?;

    // fdid → content_key (last-write-wins, matches ContentResolver behavior)
    let mut fdid_to_ck: HashMap<u32, cascette_crypto::ContentKey> = HashMap::new();
    for block in &root.blocks {
        for record in &block.records {
            fdid_to_ck.insert(record.file_data_id.get(), record.content_key);
        }
    }

    // content_key → encoding_key (first encoding key per content)
    let mut ck_to_ek: HashMap<cascette_crypto::ContentKey, cascette_crypto::EncodingKey> =
        HashMap::new();
    for page in &encoding.ckey_pages {
        for entry in &page.entries {
            if let Some(ek) = entry.encoding_keys.first() {
                ck_to_ek.insert(entry.content_key, *ek);
            }
        }
    }

    let conn = Connection::open(&cache_path)
        .map_err(|e| format!("open {}: {e}", cache_path.display()))?;

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
    .map_err(|e| format!("init resolution cache schema: {e}"))?;

    let mut insert = conn
        .prepare("INSERT INTO resolution (fdid, content_key, encoding_key) VALUES (?1, ?2, ?3)")
        .map_err(|e| format!("prepare resolution insert: {e}"))?;

    let mut n: usize = 0;
    for (fdid, ck) in &fdid_to_ck {
        if let Some(ek) = ck_to_ek.get(ck) {
            insert
                .execute((fdid, ck.as_bytes().as_ref(), ek.as_bytes().as_ref()))
                .map_err(|e| format!("insert resolution entry {fdid}: {e}"))?;
            n += 1;
        }
    }

    drop(insert);

    conn.execute(
        "INSERT INTO metadata (root_mtime, enc_mtime, schema_version) VALUES (?1, ?2, ?3)",
        (root_mtime, enc_mtime, SCHEMA_VERSION),
    )
    .map_err(|e| format!("insert resolution metadata: {e}"))?;

    conn.execute_batch("COMMIT;")
        .map_err(|e| format!("commit resolution cache: {e}"))?;

    let elapsed = t0.elapsed().as_secs_f64();
    eprintln!("Built CASC resolution cache ({n} entries) in {elapsed:.1}s");

    Ok(())
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
        Err(e) => Err(format!("query metadata: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_lookup() {
        let casc_dir = crate::paths::shared_data_path("casc");
        let cache = CascResolutionCache::open(&casc_dir).expect("failed to open cache");
        // 120191 is a well-known BLP texture FDID
        let result = cache.resolve_fdid(120191);
        assert!(result.is_some(), "expected fdid 120191 to resolve");
        let (ck, ek) = result.unwrap();
        assert_ne!(ck, [0u8; 16], "content key should not be zero");
        assert_ne!(ek, [0u8; 16], "encoding key should not be zero");
    }
}
