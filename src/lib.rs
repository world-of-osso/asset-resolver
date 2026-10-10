pub mod listfile;
pub mod listfile_cache;

mod identity;
mod paths;
mod runtime_mode;
pub use identity::AssetIdentity;
pub use runtime_mode::{
    AssetRuntimeMode, configure_runtime_mode_from_env, forbidden_casc_access_count,
    guard_casc_access, set_casc_access_hook,
};

#[cfg(feature = "casc")]
pub mod casc_cache;

#[cfg(feature = "casc")]
pub mod casc_resolver;

pub use paths::AssetResolverConfig;

pub struct CascListfileResolver {
    paths: paths::ResolverPaths,
    listfile: std::sync::OnceLock<listfile::Listfile>,
}

impl Default for CascListfileResolver {
    fn default() -> Self {
        Self::new(AssetResolverConfig::default())
    }
}

impl CascListfileResolver {
    pub fn new(config: AssetResolverConfig) -> Self {
        Self {
            paths: paths::ResolverPaths::from_config(config),
            listfile: std::sync::OnceLock::new(),
        }
    }

    /// The destination in this resolver's namespace, without reading any asset.
    pub fn cache_path(&self, destination: &std::path::Path) -> Result<std::path::PathBuf, String> {
        self.paths.scoped_cache_path(destination)
    }

    pub fn runtime_mode(&self) -> AssetRuntimeMode {
        runtime_mode::effective_runtime_mode(self.paths.runtime_mode())
    }

    fn listfile(&self) -> &listfile::Listfile {
        self.listfile
            .get_or_init(|| listfile::Listfile::from_paths(&self.paths))
    }

    /// Initialize this resolver's local CASC namespace before the first extraction.
    /// Call from a worker thread: loading TACT keys, resolution tables and archive
    /// indices can take seconds. Only resolvers with the same cache root and authored
    /// identity reuse this initialized state.
    pub fn initialize(&self) -> Result<(), String> {
        if self.runtime_mode() == AssetRuntimeMode::ExtractedOnly {
            return Ok(());
        }
        #[cfg(feature = "casc")]
        {
            return casc_resolver::initialize_with_paths(&self.paths);
        }
        #[cfg(not(feature = "casc"))]
        {
            Err("asset-resolver was built without the casc feature".to_string())
        }
    }

    pub fn resolve_bytes(&self, fdid: u32) -> Option<Vec<u8>> {
        if self.runtime_mode() == AssetRuntimeMode::ExtractedOnly {
            guard_casc_access(self.runtime_mode(), &format!("resolve_bytes FDID {fdid}"))
                .expect("raw CASC bytes requested in extracted-only mode");
        }
        #[cfg(feature = "casc")]
        {
            return casc_resolver::resolve_bytes_with_paths(&self.paths, self.listfile(), fdid);
        }
        #[cfg(not(feature = "casc"))]
        {
            let _ = fdid;
            None
        }
    }

    /// Cache an asset in this resolver's authored namespace. Missing matching
    /// builds/files retain their extraction error; unqualified files are never used
    /// by a resolver configured with an identity.
    pub fn ensure_cached_checked(
        &self,
        fdid: u32,
        out_path: &std::path::Path,
    ) -> Result<std::path::PathBuf, String> {
        let expected = self.cache_path(out_path)?;
        if expected.is_file() {
            return Ok(expected);
        }
        if self.runtime_mode() == AssetRuntimeMode::ExtractedOnly {
            return Err(self.paths.missing_extracted_asset(fdid, &expected));
        }
        #[cfg(feature = "casc")]
        {
            return casc_resolver::ensure_file_cached_checked_with_paths(
                &self.paths,
                self.listfile(),
                fdid,
                out_path,
            );
        }
        #[cfg(not(feature = "casc"))]
        {
            let _ = (fdid, out_path);
            Err("asset-resolver was built without the casc feature".to_owned())
        }
    }

    /// Legacy optional extraction: local CASC failures still log and return None.
    /// Extracted-only misses must propagate the checked error, never panic or omit it.
    pub fn ensure_cached(
        &self,
        fdid: u32,
        out_path: &std::path::Path,
    ) -> Result<Option<std::path::PathBuf>, String> {
        match self.ensure_cached_checked(fdid, out_path) {
            Ok(path) => Ok(Some(path)),
            Err(error) if self.runtime_mode() == AssetRuntimeMode::ExtractedOnly => Err(error),
            Err(error) => {
                eprintln!("asset-cache extraction failed: {error}");
                Ok(None)
            }
        }
    }

    pub fn resolve_path(&self, fdid: u32) -> Option<String> {
        self.listfile().lookup_fdid(fdid).map(str::to_owned)
    }

    pub fn lookup_path(&self, path: &str) -> Option<u32> {
        self.listfile().lookup_path(path)
    }
}

#[cfg(feature = "casc")]
pub use casc_resolver::{
    FrozenArchiveReader, ensure_file_cached_at_path, resolve_bytes, wow_data_path, wow_install_path,
};
pub use listfile::{CachedListfile, Listfile, lookup_fdid, lookup_path};

#[cfg(test)]
mod tests {
    use super::{AssetResolverConfig, CascListfileResolver};
    use std::path::Path;

    #[test]
    fn resolver_creation_accepts_explicit_locations() {
        let resolver = CascListfileResolver::new(
            AssetResolverConfig::new()
                .with_data_root("/tmp/resolver-data")
                .with_shared_data_root("/tmp/resolver-shared")
                .with_cache_root("/tmp/resolver-cache"),
        );

        assert_eq!(
            resolver.paths.source_data_root(),
            Path::new("/tmp/resolver-data")
        );
        assert_eq!(
            resolver.paths.shared_data_root(),
            Path::new("/tmp/resolver-shared")
        );
        assert_eq!(
            resolver.paths.cache_root(),
            Path::new("/tmp/resolver-cache")
        );
    }
}
