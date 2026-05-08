use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, UNIX_EPOCH};

use cascette_crypto::{ContentKey, EncodingKey};
use rusqlite::{Connection, OpenFlags};

const SCHEMA_VERSION: i64 = 2;
type FdidToContentKeyMap = HashMap<u32, ContentKey>;
type ContentToEncodingKeyMap = HashMap<ContentKey, EncodingKey>;
type TvfsResolutionMap = HashMap<u32, (ContentKey, EncodingKey)>;

pub struct CascResolutionCache {
    conn: Mutex<Connection>,
}

impl CascResolutionCache {
    /// Open an existing resolution cache in read-only mode.
    ///
    /// Returns an error if the cache doesn't exist or is stale relative to
    /// root.bin / encoding.bin. Run [`build_resolution_cache`] first (via `casc_refresh`).
    pub fn open(casc_dir: &Path) -> Result<Self, String> {
        let (cache_path, root_path, enc_path, vfs_path) = resolution_paths(casc_dir);

        if !cache_path.exists() {
            return Err(format!(
                "{} not found (run `casc_refresh` to build it)",
                cache_path.display()
            ));
        }

        let root_mtime = file_mtime(&root_path)?;
        let enc_mtime = file_mtime(&enc_path)?;
        let vfs_mtime = optional_file_mtime(&vfs_path)?;

        if !cache_is_fresh(&cache_path, root_mtime, enc_mtime, vfs_mtime)? {
            return Err(format!(
                "{} is stale (run `casc_refresh` to rebuild it)",
                cache_path.display()
            ));
        }

        let conn = Connection::open_with_flags(
            &cache_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
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

    /// Persist FDID resolutions discovered lazily from TVFS manifests.
    pub fn remember_resolutions(&self, resolutions: &TvfsResolutionMap) -> Result<usize, String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| format!("begin TVFS resolution transaction: {e}"))?;
        let mut insert = tx
            .prepare(
                "INSERT OR REPLACE INTO resolution (fdid, content_key, encoding_key) VALUES (?1, ?2, ?3)",
            )
            .map_err(|e| format!("prepare TVFS resolution cache insert: {e}"))?;
        let mut inserted = 0usize;
        for (&fdid, (content_key, encoding_key)) in resolutions {
            insert
                .execute((
                    fdid,
                    content_key.as_bytes().as_ref(),
                    encoding_key.as_bytes().as_ref(),
                ))
                .map_err(|e| format!("cache TVFS resolution entry {fdid}: {e}"))?;
            inserted += 1;
        }
        drop(insert);
        tx.commit()
            .map_err(|e| format!("commit TVFS resolution cache: {e}"))?;
        Ok(inserted)
    }
}

pub fn resolution_cache_is_fresh(casc_dir: &Path) -> Result<bool, String> {
    let (cache_path, root_path, enc_path, vfs_path) = resolution_paths(casc_dir);
    if !cache_path.exists() {
        return Ok(false);
    }

    let root_mtime = file_mtime(&root_path)?;
    let enc_mtime = file_mtime(&enc_path)?;
    let vfs_mtime = optional_file_mtime(&vfs_path)?;
    cache_is_fresh(&cache_path, root_mtime, enc_mtime, vfs_mtime)
}

/// Build (or rebuild) the resolution SQLite cache from root.bin + encoding.bin.
///
/// Called by `casc_refresh` after writing the binary files.
pub fn build_resolution_cache(casc_dir: &Path) -> Result<(), String> {
    let (cache_path, root_path, enc_path, vfs_path) = resolution_paths(casc_dir);
    let root_mtime = file_mtime(&root_path)?;
    let enc_mtime = file_mtime(&enc_path)?;
    let vfs_mtime = optional_file_mtime(&vfs_path)?;
    let started_at = Instant::now();
    let (fdid_to_ck, ck_to_ek, tvfs_resolutions) =
        build_resolution_maps(&root_path, &enc_path, &vfs_path)?;
    let conn = open_resolution_cache(&cache_path)?;
    init_resolution_schema(&conn)?;
    let inserted_rows = insert_resolution_rows(&conn, &fdid_to_ck, &ck_to_ek, &tvfs_resolutions)?;
    insert_resolution_metadata(&conn, root_mtime, enc_mtime, vfs_mtime)?;
    commit_resolution_cache(&conn)?;
    log_resolution_cache_build(inserted_rows, started_at);
    Ok(())
}

fn resolution_paths(casc_dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    (
        casc_dir.join("resolution.sqlite"),
        casc_dir.join("root.bin"),
        casc_dir.join("encoding.bin"),
        casc_dir.join("vfs-root.bin"),
    )
}

fn build_resolution_maps(
    root_path: &Path,
    enc_path: &Path,
    vfs_path: &Path,
) -> Result<
    (
        FdidToContentKeyMap,
        ContentToEncodingKeyMap,
        TvfsResolutionMap,
    ),
    String,
> {
    let root_data = read_cache_file(root_path)?;
    let enc_data = read_cache_file(enc_path)?;
    let root = cascette_formats::root::RootFile::parse(&root_data)
        .map_err(|e| format!("parse root.bin: {e}"))?;
    let encoding = cascette_formats::encoding::EncodingFile::parse(&enc_data)
        .map_err(|e| format!("parse encoding.bin: {e}"))?;
    let fdid_to_ck = collect_fdid_to_content_keys(&root);
    let ck_to_ek = collect_content_to_encoding_keys(&encoding);
    let tvfs_resolutions = if vfs_path.exists() {
        let vfs_data = read_cache_file(vfs_path)?;
        crate::tvfs_cache::collect_wow_tvfs_resolutions(&vfs_data)
            .map_err(|e| format!("parse vfs-root.bin: {e}"))?
    } else {
        HashMap::new()
    };
    Ok((fdid_to_ck, ck_to_ek, tvfs_resolutions))
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
             vfs_mtime INTEGER NOT NULL,
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
    tvfs_resolutions: &TvfsResolutionMap,
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

    let mut insert_tvfs = conn
        .prepare("INSERT OR REPLACE INTO resolution (fdid, content_key, encoding_key) VALUES (?1, ?2, ?3)")
        .map_err(|e| format!("prepare TVFS resolution insert: {e}"))?;
    for (&fdid, (content_key, encoding_key)) in tvfs_resolutions {
        insert_tvfs
            .execute((
                fdid,
                content_key.as_bytes().as_ref(),
                encoding_key.as_bytes().as_ref(),
            ))
            .map_err(|e| format!("insert TVFS resolution entry {fdid}: {e}"))?;
        inserted_rows += 1;
    }

    drop(insert);
    drop(insert_tvfs);
    Ok(inserted_rows)
}

fn insert_resolution_metadata(
    conn: &Connection,
    root_mtime: i64,
    enc_mtime: i64,
    vfs_mtime: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO metadata (root_mtime, enc_mtime, vfs_mtime, schema_version) VALUES (?1, ?2, ?3, ?4)",
        (root_mtime, enc_mtime, vfs_mtime, SCHEMA_VERSION),
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

fn optional_file_mtime(path: &Path) -> Result<i64, String> {
    if path.exists() {
        file_mtime(path)
    } else {
        Ok(0)
    }
}

fn cache_is_fresh(
    cache_path: &Path,
    root_mtime: i64,
    enc_mtime: i64,
    vfs_mtime: i64,
) -> Result<bool, String> {
    let conn = Connection::open_with_flags(
        cache_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open {}: {e}", cache_path.display()))?;

    let mut stmt = match conn
        .prepare("SELECT root_mtime, enc_mtime, vfs_mtime, schema_version FROM metadata LIMIT 1")
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
            row.get::<_, i64>(3)?,
        ))
    });

    match row {
        Ok((stored_root, stored_enc, stored_vfs, stored_version)) => Ok(stored_root == root_mtime
            && stored_enc == enc_mtime
            && stored_vfs == vfs_mtime
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
