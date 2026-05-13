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
        let local = listfile_cache::load_local_cache(&local_cache_path).unwrap_or_else(|err| {
            eprintln!(
                "Failed to load local listfile cache {}: {err}",
                local_cache_path.display()
            );
            CachedListfile::default()
        });
        Self {
            community_path,
            community_cache_path,
            local_cache_path,
            community_cache: Mutex::new(None),
            community_error_logged: Mutex::new(false),
            local: Mutex::new(local),
        }
    }

    pub fn lookup_fdid(&self, fdid: u32) -> Option<&'static str> {
        if let Some(path) = self.local.lock().unwrap().by_fdid.get(&fdid).copied() {
            return Some(path);
        }
        self.lookup_community_fdid(fdid)
    }

    pub fn lookup_path(&self, path: &str) -> Option<u32> {
        let normalized = path.to_ascii_lowercase();
        if let Some(fdid) = self.local.lock().unwrap().by_path.get(&normalized).copied() {
            return Some(fdid);
        }
        self.lookup_community_path(path)
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
        if self.remember_in_memory(fdid, path) {
            return;
        }
        if let Err(err) =
            listfile_cache::remember_local_cache_entry(&self.local_cache_path, fdid, path)
        {
            eprintln!("Failed to persist listfile cache entry {fdid}: {err}");
        }
    }

    fn remember_in_memory(&self, fdid: u32, path: &'static str) -> bool {
        let normalized = path.to_ascii_lowercase();
        let mut local = self.local.lock().unwrap();
        let known_fdid = local.by_fdid.contains_key(&fdid);
        let known_path = local.by_path.contains_key(&normalized);
        if known_fdid && known_path {
            return true;
        }
        local.by_fdid.entry(fdid).or_insert(path);
        local.by_path.entry(normalized).or_insert(fdid);
        false
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
