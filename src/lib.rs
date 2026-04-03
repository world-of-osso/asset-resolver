pub mod listfile;
pub mod listfile_cache;

mod paths;

#[cfg(feature = "casc")]
pub mod casc_resolver;

#[derive(Debug, Default, Clone, Copy)]
pub struct CascListfileResolver;

impl CascListfileResolver {
    pub fn resolve_bytes(&self, fdid: u32) -> Option<Vec<u8>> {
        #[cfg(feature = "casc")]
        {
            return casc_resolver::resolve_bytes(fdid);
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
            return casc_resolver::ensure_file_cached_at_path(fdid, out_path);
        }
        #[cfg(not(feature = "casc"))]
        {
            let _ = (fdid, out_path);
            None
        }
    }

    pub fn resolve_path(&self, fdid: u32) -> Option<String> {
        lookup_fdid(fdid).map(str::to_owned)
    }

    pub fn lookup_path(&self, path: &str) -> Option<u32> {
        lookup_path(path)
    }
}

#[cfg(feature = "casc")]
pub use casc_resolver::{ensure_file_cached_at_path, resolve_bytes};
pub use listfile::{CachedListfile, Listfile, lookup_fdid, lookup_path};
