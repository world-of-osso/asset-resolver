pub mod listfile;
pub mod listfile_cache;

mod paths;

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

    fn listfile(&self) -> &listfile::Listfile {
        self.listfile
            .get_or_init(|| listfile::Listfile::from_paths(&self.paths))
    }

    pub fn resolve_bytes(&self, fdid: u32) -> Option<Vec<u8>> {
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

    pub fn ensure_cached(
        &self,
        fdid: u32,
        out_path: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        #[cfg(feature = "casc")]
        {
            return casc_resolver::ensure_file_cached_at_path_with_paths(
                &self.paths,
                self.listfile(),
                fdid,
                out_path,
            );
        }
        #[cfg(not(feature = "casc"))]
        {
            let _ = (fdid, out_path);
            None
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
    ensure_file_cached_at_path, resolve_bytes, wow_data_path, wow_install_path,
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
