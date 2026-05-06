use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OpenFlags};

use crate::listfile::CachedListfile;

const COMMUNITY_LISTFILE_CACHE_PATH: &str = "community-listfile.sqlite";

pub fn cache_path() -> PathBuf {
    crate::paths::shared_data_path(COMMUNITY_LISTFILE_CACHE_PATH)
}

pub struct CommunityCache {
    conn: Connection,
}

impl CommunityCache {
    pub fn open(cache_path: &Path, source_path: &Path) -> Result<Self, String> {
        let cache_path = ensure_cache(cache_path, source_path)?;
        let conn = Connection::open_with_flags(
            &cache_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|err| format!("open {}: {err}", cache_path.display()))?;
        Ok(Self { conn })
    }

    pub fn lookup_fdid(&self, fdid: u32) -> Result<Option<String>, String> {
        query_fdid(&self.conn, fdid)
    }

    pub fn lookup_path(&self, path: &str) -> Result<Option<(u32, String)>, String> {
        query_path(&self.conn, path)
    }
}

pub fn load_local_cache(cache_path: &Path) -> Result<CachedListfile, String> {
    if !cache_path.exists() {
        return Ok(CachedListfile::default());
    }
    let conn = Connection::open_with_flags(
        cache_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|err| format!("open {}: {err}", cache_path.display()))?;
    let mut stmt = match conn.prepare("SELECT fdid, path FROM local_listfile_entries ORDER BY fdid")
    {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table") =>
        {
            return Ok(CachedListfile::default());
        }
        Err(err) => return Err(format!("prepare local_listfile_entries query: {err}")),
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|err| format!("query local_listfile_entries: {err}"))?;
    let mut by_fdid = HashMap::new();
    let mut by_path = HashMap::new();
    for row in rows {
        let (fdid, path) = row.map_err(|err| format!("read local_listfile_entries row: {err}"))?;
        let leaked = Box::leak(path.into_boxed_str()) as &'static str;
        by_fdid.insert(fdid, leaked);
        by_path.insert(leaked.to_ascii_lowercase(), fdid);
    }
    Ok(CachedListfile { by_fdid, by_path })
}

pub fn remember_local_cache_entry(cache_path: &Path, fdid: u32, path: &str) -> Result<(), String> {
    let Some(parent) = cache_path.parent() else {
        return Err(format!("missing parent for {}", cache_path.display()));
    };
    std::fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    let conn = Connection::open(cache_path)
        .map_err(|err| format!("open {}: {err}", cache_path.display()))?;
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS local_listfile_entries (
             fdid INTEGER PRIMARY KEY,
             path TEXT NOT NULL,
             lower_path TEXT NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_local_listfile_entries_lower_path
         ON local_listfile_entries(lower_path);",
    )
    .map_err(|err| format!("init local_listfile_entries cache: {err}"))?;
    conn.execute(
        "INSERT OR REPLACE INTO local_listfile_entries (fdid, path, lower_path) VALUES (?1, ?2, ?3)",
        (fdid, path, path.to_ascii_lowercase()),
    )
    .map_err(|err| format!("insert local_listfile_entries row {fdid}: {err}"))?;
    conn.execute_batch("COMMIT;")
        .map_err(|err| format!("commit local_listfile_entries cache: {err}"))?;
    Ok(())
}

pub fn lookup_local_fdid(cache_path: &Path, fdid: u32) -> Result<Option<String>, String> {
    if !cache_path.exists() {
        return Ok(None);
    }
    let conn = open_local_cache(cache_path)?;
    conn.query_row(
        "SELECT path FROM local_listfile_entries WHERE fdid = ?1",
        [fdid],
        |row| row.get::<_, String>(0),
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        _ => Err(format!(
            "query local_listfile_entries by fdid {fdid}: {err}"
        )),
    })
}

pub fn lookup_local_path(cache_path: &Path, path: &str) -> Result<Option<(u32, String)>, String> {
    if !cache_path.exists() {
        return Ok(None);
    }
    let conn = open_local_cache(cache_path)?;
    let normalized = path.to_ascii_lowercase();
    conn.query_row(
        "SELECT fdid, path FROM local_listfile_entries WHERE lower_path = ?1",
        [normalized],
        |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)),
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        _ => Err(format!(
            "query local_listfile_entries by path `{path}`: {err}"
        )),
    })
}

pub fn lookup_fdid(
    cache_path: &Path,
    source_path: &Path,
    fdid: u32,
) -> Result<Option<String>, String> {
    CommunityCache::open(cache_path, source_path)?.lookup_fdid(fdid)
}

pub fn lookup_path(
    cache_path: &Path,
    source_path: &Path,
    path: &str,
) -> Result<Option<(u32, String)>, String> {
    CommunityCache::open(cache_path, source_path)?.lookup_path(path)
}

fn query_fdid(conn: &Connection, fdid: u32) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT path FROM listfile_entries WHERE fdid = ?1",
        [fdid],
        |row| row.get::<_, String>(0),
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        _ => Err(format!("query listfile_entries by fdid {fdid}: {err}")),
    })
}

fn query_path(conn: &Connection, path: &str) -> Result<Option<(u32, String)>, String> {
    let normalized = path.to_ascii_lowercase();
    conn.query_row(
        "SELECT fdid, path FROM listfile_entries WHERE lower_path = ?1",
        [normalized],
        |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)),
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        _ => Err(format!("query listfile_entries by path `{path}`: {err}")),
    })
}

fn open_local_cache(cache_path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(
        cache_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|err| format!("open {}: {err}", cache_path.display()))
}

fn ensure_cache(cache_path: &Path, source_path: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    let conn = Connection::open(cache_path)
        .map_err(|err| format!("open {}: {err}", cache_path.display()))?;
    if !source_path.exists() {
        if cache_has_entries(&conn)? {
            return Ok(cache_path.to_path_buf());
        }
        return Err(format!("missing {}", source_path.display()));
    }
    if !cache_is_fresh(&conn, source_path)? {
        rebuild_cache(&conn, source_path)?;
    }
    Ok(cache_path.to_path_buf())
}

fn cache_has_entries(conn: &Connection) -> Result<bool, String> {
    conn.query_row("SELECT 1 FROM listfile_entries LIMIT 1", [], |_| Ok(()))
        .map(|()| true)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            rusqlite::Error::SqliteFailure(_, Some(message))
                if message.contains("no such table") =>
            {
                Ok(false)
            }
            _ => Err(format!("query listfile_entries availability: {err}")),
        })
}

fn cache_is_fresh(conn: &Connection, source_path: &Path) -> Result<bool, String> {
    let mut stmt = match conn.prepare("SELECT source_path, source_mtime FROM metadata LIMIT 1") {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table") =>
        {
            return Ok(false);
        }
        Err(err) => return Err(format!("prepare metadata query: {err}")),
    };
    let row = stmt.query_row([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    });
    let (recorded_path, recorded_mtime) = match row {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(false),
        Err(err) => return Err(format!("query metadata: {err}")),
    };
    Ok(recorded_path == source_path.to_string_lossy()
        && recorded_mtime == source_mtime(source_path)?)
}

fn rebuild_cache(conn: &Connection, source_path: &Path) -> Result<(), String> {
    init_cache_schema(conn)?;
    import_rows(conn, source_path)?;
    record_metadata(conn, source_path)?;
    conn.execute_batch("COMMIT;")
        .map_err(|err| format!("commit listfile cache: {err}"))?;
    Ok(())
}

fn init_cache_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "BEGIN;
         DROP TABLE IF EXISTS metadata;
         DROP TABLE IF EXISTS listfile_entries;
         CREATE TABLE metadata (
             source_path TEXT NOT NULL,
             source_mtime INTEGER NOT NULL
         );
         CREATE TABLE listfile_entries (
             fdid INTEGER PRIMARY KEY,
             path TEXT NOT NULL,
             lower_path TEXT NOT NULL
         );
         CREATE UNIQUE INDEX idx_listfile_entries_lower_path ON listfile_entries(lower_path);",
    )
    .map_err(|err| format!("init listfile cache: {err}"))
}

fn import_rows(conn: &Connection, source_path: &Path) -> Result<(), String> {
    let file = std::fs::File::open(source_path)
        .map_err(|err| format!("open {}: {err}", source_path.display()))?;
    let reader = BufReader::new(file);
    let mut insert = conn
        .prepare(
            "INSERT OR REPLACE INTO listfile_entries (fdid, path, lower_path) VALUES (?1, ?2, ?3)",
        )
        .map_err(|err| format!("prepare listfile_entries insert: {err}"))?;
    for line in reader.lines() {
        let line = line.map_err(|err| format!("read {} row: {err}", source_path.display()))?;
        let Some((fdid_str, path)) = line.split_once(';') else {
            continue;
        };
        let Ok(fdid) = fdid_str.parse::<u32>() else {
            continue;
        };
        insert
            .execute((fdid, path, path.to_ascii_lowercase()))
            .map_err(|err| format!("insert listfile entry {fdid}: {err}"))?;
    }
    Ok(())
}

fn record_metadata(conn: &Connection, source_path: &Path) -> Result<(), String> {
    conn.execute(
        "INSERT INTO metadata (source_path, source_mtime) VALUES (?1, ?2)",
        (
            source_path.to_string_lossy().to_string(),
            source_mtime(source_path)?,
        ),
    )
    .map_err(|err| format!("insert listfile metadata: {err}"))?;
    Ok(())
}

fn source_mtime(path: &Path) -> Result<i64, String> {
    let modified = std::fs::metadata(path)
        .map_err(|err| format!("stat {}: {err}", path.display()))?
        .modified()
        .map_err(|err| format!("mtime {}: {err}", path.display()))?;
    Ok(modified
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("mtime epoch {}: {err}", path.display()))?
        .as_secs() as i64)
}
