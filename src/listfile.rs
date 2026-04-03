use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::listfile_cache;

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
    pub local: Mutex<CachedListfile>,
}

pub fn lookup_fdid(fdid: u32) -> Option<&'static str> {
    get().lookup_fdid(fdid)
}

pub fn lookup_path(path: &str) -> Option<u32> {
    get().lookup_path(path)
}

fn get() -> &'static Listfile {
    LISTFILE.get_or_init(|| {
        Listfile::new(
            crate::paths::resolve_data_path("community-listfile.csv"),
            listfile_cache::cache_path(),
            crate::paths::shared_data_path("local-listfile-cache.sqlite"),
        )
    })
}

impl Listfile {
    pub fn new(
        community_path: PathBuf,
        community_cache_path: PathBuf,
        local_cache_path: PathBuf,
    ) -> Self {
        Self {
            community_path,
            community_cache_path,
            local_cache_path,
            local: Mutex::new(CachedListfile::default()),
        }
    }

    pub fn lookup_fdid(&self, fdid: u32) -> Option<&'static str> {
        if let Some(path) = self.local.lock().unwrap().by_fdid.get(&fdid).copied() {
            return Some(path);
        }
        if let Some(path) = self.lookup_local_fdid(fdid) {
            return Some(path);
        }
        self.lookup_community_fdid(fdid)
    }

    pub fn lookup_path(&self, path: &str) -> Option<u32> {
        let normalized = path.to_ascii_lowercase();
        if let Some(fdid) = self.local.lock().unwrap().by_path.get(&normalized).copied() {
            return Some(fdid);
        }
        if let Some(fdid) = self.lookup_local_path(path) {
            return Some(fdid);
        }
        self.lookup_community_path(path)
    }

    fn lookup_community_fdid(&self, fdid: u32) -> Option<&'static str> {
        let path = match listfile_cache::lookup_fdid(
            &self.community_cache_path,
            &self.community_path,
            fdid,
        ) {
            Ok(path) => path?,
            Err(err) => {
                eprintln!("Failed listfile fdid lookup {fdid}: {err}");
                return None;
            }
        };
        let leaked = Box::leak(path.into_boxed_str()) as &'static str;
        self.remember(fdid, leaked);
        Some(leaked)
    }

    fn lookup_community_path(&self, path: &str) -> Option<u32> {
        let (fdid, resolved_path) = self.resolve_community_path(path)?;
        let leaked = Box::leak(resolved_path.into_boxed_str()) as &'static str;
        self.remember(fdid, leaked);
        Some(fdid)
    }

    fn resolve_community_path(&self, path: &str) -> Option<(u32, String)> {
        match listfile_cache::lookup_path(&self.community_cache_path, &self.community_path, path) {
            Ok(row) => row,
            Err(err) => {
                eprintln!("Failed listfile path lookup `{path}`: {err}");
                None
            }
        }
    }

    fn lookup_local_fdid(&self, fdid: u32) -> Option<&'static str> {
        let path = match listfile_cache::lookup_local_fdid(&self.local_cache_path, fdid) {
            Ok(path) => path?,
            Err(err) => {
                eprintln!("Failed local listfile fdid lookup {fdid}: {err}");
                return None;
            }
        };
        let leaked = Box::leak(path.into_boxed_str()) as &'static str;
        self.remember_in_memory(fdid, leaked);
        Some(leaked)
    }

    fn lookup_local_path(&self, path: &str) -> Option<u32> {
        let (fdid, resolved_path) =
            match listfile_cache::lookup_local_path(&self.local_cache_path, path) {
                Ok(row) => row?,
                Err(err) => {
                    eprintln!("Failed local listfile path lookup `{path}`: {err}");
                    return None;
                }
            };
        let leaked = Box::leak(resolved_path.into_boxed_str()) as &'static str;
        self.remember_in_memory(fdid, leaked);
        Some(fdid)
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
}
