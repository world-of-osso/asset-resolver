use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::listfile_cache;
use crate::paths::ResolverPaths;

static LISTFILE: OnceLock<Listfile> = OnceLock::new();

#[derive(Default)]
pub struct CachedListfile {
    pub by_fdid: HashMap<u32, &'static str>,
    pub by_path: HashMap<String, u32>,
}

pub struct Listfile {
    community_path: PathBuf,
    community_cache_path: PathBuf,
    local_cache_path: PathBuf,
    community_cache: Mutex<Option<listfile_cache::CommunityCache>>,
    community_error_logged: Mutex<bool>,
    local_error_logged: Mutex<bool>,
    pub local: Mutex<CachedListfile>,
}

pub fn lookup_fdid(fdid: u32) -> Option<&'static str> {
    get().lookup_fdid(fdid)
}

pub fn lookup_path(path: &str) -> Option<u32> {
    get().lookup_path(path)
}

pub(crate) fn get_default() -> &'static Listfile {
    LISTFILE.get_or_init(|| Listfile::from_paths(crate::paths::default_paths()))
}

fn get() -> &'static Listfile {
    get_default()
}

fn community_listfile_path(paths: &ResolverPaths) -> PathBuf {
    let community_path = paths.resolve_data_path("community-listfile.csv");
    if community_path.exists() {
        return community_path;
    }

    let limited_path = paths.resolve_data_path("wow-ui-sim-listfile.csv");
    if limited_path.exists() {
        return limited_path;
    }

    community_path
}

impl Listfile {
    pub(crate) fn from_paths(paths: &ResolverPaths) -> Self {
        Self::new(
            community_listfile_path(paths),
            listfile_cache::cache_path(paths),
            paths.shared_data_path("local-listfile-cache.sqlite"),
        )
    }

    pub fn new(
        community_path: PathBuf,
        community_cache_path: PathBuf,
        local_cache_path: PathBuf,
    ) -> Self {
        Self {
            community_path,
            community_cache_path,
            local_cache_path,
            community_cache: Mutex::new(None),
            community_error_logged: Mutex::new(false),
            local_error_logged: Mutex::new(false),
            local: Mutex::new(CachedListfile::default()),
        }
    }

    pub fn lookup_fdid(&self, fdid: u32) -> Option<&'static str> {
        self.lookup_local_fdid(fdid)
            .or_else(|| self.lookup_community_fdid(fdid))
    }

    pub fn lookup_path(&self, path: &str) -> Option<u32> {
        self.lookup_local_path(path)
            .or_else(|| self.lookup_community_path(path))
    }

    fn lookup_local_fdid(&self, fdid: u32) -> Option<&'static str> {
        if let Some(path) = self.local.lock().unwrap().by_fdid.get(&fdid).copied() {
            return Some(path);
        }
        let path = match listfile_cache::lookup_local_fdid(&self.local_cache_path, fdid) {
            Ok(path) => path?,
            Err(err) => {
                self.log_local_error_once(&err);
                return None;
            }
        };
        Some(self.cache_local_path(fdid, path))
    }

    fn lookup_local_path(&self, path: &str) -> Option<u32> {
        let normalized = path.to_ascii_lowercase();
        if let Some(fdid) = self.local.lock().unwrap().by_path.get(&normalized).copied() {
            return Some(fdid);
        }
        let (fdid, resolved_path) =
            match listfile_cache::lookup_local_path(&self.local_cache_path, path) {
                Ok(row) => row?,
                Err(err) => {
                    self.log_local_error_once(&err);
                    return None;
                }
            };
        self.cache_local_path(fdid, resolved_path);
        Some(fdid)
    }

    fn cache_local_path(&self, fdid: u32, path: String) -> &'static str {
        let leaked = Box::leak(path.into_boxed_str()) as &'static str;
        self.cache_in_memory(fdid, leaked).0
    }

    fn lookup_community_fdid(&self, fdid: u32) -> Option<&'static str> {
        let path = match self.with_community_cache(|cache| cache.lookup_fdid(fdid)) {
            Ok(path) => path?,
            Err(err) => {
                self.log_community_error_once(&err);
                return None;
            }
        };
        Some(self.cache_path_for_fdid(fdid, path))
    }

    fn lookup_community_path(&self, path: &str) -> Option<u32> {
        let (fdid, resolved_path) = self.resolve_community_path(path)?;
        self.cache_path_for_fdid(fdid, resolved_path);
        Some(fdid)
    }

    fn cache_path_for_fdid(&self, fdid: u32, path: String) -> &'static str {
        let leaked = Box::leak(path.into_boxed_str()) as &'static str;
        self.remember(fdid, leaked);
        leaked
    }

    fn resolve_community_path(&self, path: &str) -> Option<(u32, String)> {
        match self.with_community_cache(|cache| cache.lookup_path(path)) {
            Ok(row) => row,
            Err(err) => {
                self.log_community_error_once(&err);
                None
            }
        }
    }

    fn with_community_cache<T>(
        &self,
        query: impl FnOnce(&listfile_cache::CommunityCache) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut cache = self.community_cache.lock().unwrap();
        if cache.is_none() {
            *cache = Some(listfile_cache::CommunityCache::open(
                &self.community_cache_path,
                &self.community_path,
            )?);
        }
        query(
            cache
                .as_ref()
                .expect("community cache was just initialized"),
        )
    }

    fn remember(&self, fdid: u32, path: &'static str) {
        let (_, already_cached) = self.cache_in_memory(fdid, path);
        if already_cached {
            return;
        }
        if let Err(err) =
            listfile_cache::remember_local_cache_entry(&self.local_cache_path, fdid, path)
        {
            eprintln!("Failed to persist listfile cache entry {fdid}: {err}");
        }
    }

    fn cache_in_memory(&self, fdid: u32, path: &'static str) -> (&'static str, bool) {
        let normalized = path.to_ascii_lowercase();
        let mut local = self.local.lock().unwrap();
        let already_cached =
            local.by_fdid.contains_key(&fdid) && local.by_path.contains_key(&normalized);
        let cached_path = *local.by_fdid.entry(fdid).or_insert(path);
        local.by_path.entry(normalized).or_insert(fdid);
        (cached_path, already_cached)
    }

    fn log_local_error_once(&self, err: &str) {
        let mut logged = self.local_error_logged.lock().unwrap();
        if *logged {
            return;
        }
        *logged = true;
        eprintln!(
            "Local listfile cache unavailable {}: {err}",
            self.local_cache_path.display()
        );
    }

    fn log_community_error_once(&self, err: &str) {
        if err.starts_with("missing ") {
            return;
        }
        let mut logged = self.community_error_logged.lock().unwrap();
        if *logged {
            return;
        }
        *logged = true;
        eprintln!("Community listfile unavailable; using local listfile cache only: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::{
        fs, io,
        path::{Path, PathBuf},
    };

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let base = std::env::temp_dir();
            loop {
                let unique_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock must be after the Unix epoch")
                    .as_nanos();
                let path = base.join(format!(
                    "asset-resolver-listfile-test-{}-{timestamp}-{unique_id}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self { path },
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(err) => panic!("create test directory {}: {err}", path.display()),
                }
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).expect("remove test directory");
        }
    }

    fn assert_local_maps_are_empty(listfile: &Listfile) {
        let local = listfile.local.lock().unwrap();
        assert!(local.by_fdid.is_empty());
        assert!(local.by_path.is_empty());
    }

    fn write_community_row(path: &Path, fdid: u32, community_path: &str) {
        fs::write(path, format!("{fdid};{community_path}\n")).unwrap();
    }

    #[test]
    fn new_leaves_local_maps_empty_then_lookup_fdid_loads_sqlite_row() {
        let temp = TestTempDir::new();
        let local_cache = temp.path("local.sqlite");
        let fdid = 1_234_567;
        let local_path = "Interface/AddOns/Local.lua";
        listfile_cache::remember_local_cache_entry(&local_cache, fdid, local_path).unwrap();

        let listfile = Listfile::new(
            temp.path("community.csv"),
            temp.path("community.sqlite"),
            local_cache,
        );
        assert_local_maps_are_empty(&listfile);

        assert_eq!(listfile.lookup_fdid(fdid), Some(local_path));
        let local = listfile.local.lock().unwrap();
        assert_eq!(local.by_fdid.get(&fdid).copied(), Some(local_path));
        assert_eq!(
            local.by_path.get(&local_path.to_ascii_lowercase()),
            Some(&fdid)
        );
    }

    #[test]
    fn new_leaves_local_maps_empty_then_case_insensitive_lookup_path_loads_sqlite_row() {
        let temp = TestTempDir::new();
        let local_cache = temp.path("local.sqlite");
        let fdid = 2_345_678;
        let local_path = "Interface/AddOns/Local.lua";
        listfile_cache::remember_local_cache_entry(&local_cache, fdid, local_path).unwrap();

        let listfile = Listfile::new(
            temp.path("community.csv"),
            temp.path("community.sqlite"),
            local_cache,
        );
        assert_local_maps_are_empty(&listfile);

        assert_eq!(
            listfile.lookup_path("interface/addons/local.lua"),
            Some(fdid)
        );
        let local = listfile.local.lock().unwrap();
        assert_eq!(local.by_fdid.get(&fdid).copied(), Some(local_path));
        assert_eq!(local.by_path.get("interface/addons/local.lua"), Some(&fdid));
    }

    #[test]
    fn existing_sqlite_without_local_table_behaves_as_empty_cache() {
        let temp = TestTempDir::new();
        let local_cache = temp.path("local.sqlite");
        Connection::open(&local_cache).unwrap();

        let listfile = Listfile::new(
            temp.path("community.csv"),
            temp.path("community.sqlite"),
            local_cache,
        );
        assert_local_maps_are_empty(&listfile);
        assert_eq!(listfile.lookup_fdid(3_456_789), None);
        assert_eq!(listfile.lookup_path("interface/addons/missing.lua"), None);
    }

    #[test]
    fn local_sqlite_row_wins_over_conflicting_community_cache_row() {
        let temp = TestTempDir::new();
        let community_path = temp.path("community.csv");
        let community_cache = temp.path("community.sqlite");
        let local_cache = temp.path("local.sqlite");
        let fdid = 4_567_890;
        let local_path = "Interface/AddOns/Local.lua";
        let community_row_path = "Interface/AddOns/Community.lua";
        write_community_row(&community_path, fdid, community_row_path);
        listfile_cache::CommunityCache::open(&community_cache, &community_path).unwrap();
        listfile_cache::remember_local_cache_entry(&local_cache, fdid, local_path).unwrap();

        let community =
            listfile_cache::CommunityCache::open(&community_cache, &community_path).unwrap();
        assert_eq!(
            community.lookup_fdid(fdid).unwrap().as_deref(),
            Some(community_row_path)
        );

        let listfile = Listfile::new(community_path, community_cache, local_cache);
        assert_eq!(listfile.lookup_fdid(fdid), Some(local_path));
    }
}
