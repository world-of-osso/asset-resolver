use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DEFAULT_PATHS: OnceLock<ResolverPaths> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct AssetResolverConfig {
    source_data_root: Option<PathBuf>,
    shared_data_root: Option<PathBuf>,
    cache_root: Option<PathBuf>,
}

impl AssetResolverConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_data_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.source_data_root = Some(path.into());
        self
    }

    pub fn with_shared_data_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.shared_data_root = Some(path.into());
        self
    }

    pub fn with_cache_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_root = Some(path.into());
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolverPaths {
    source_data_root: PathBuf,
    shared_data_root: PathBuf,
    cache_root: PathBuf,
}

impl ResolverPaths {
    pub(crate) fn from_config(config: AssetResolverConfig) -> Self {
        let source_data_root = config
            .source_data_root
            .or_else(env_source_data_root)
            .unwrap_or_else(default_source_data_root);
        let cache_root = config
            .cache_root
            .or_else(env_cache_root)
            .unwrap_or_else(|| default_cache_root().join("asset-resolver"));
        let shared_data_root = config
            .shared_data_root
            .or_else(env_shared_data_root)
            .unwrap_or_else(|| cache_root.join("data"));

        Self {
            source_data_root,
            shared_data_root,
            cache_root,
        }
    }

    #[cfg(test)]
    pub(crate) fn source_data_root(&self) -> &Path {
        &self.source_data_root
    }

    #[cfg(test)]
    pub(crate) fn shared_data_root(&self) -> &Path {
        &self.shared_data_root
    }

    #[cfg(test)]
    pub(crate) fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub(crate) fn resolve_data_path(&self, relative: impl AsRef<Path>) -> PathBuf {
        let relative = relative.as_ref();
        let source_path = self.source_data_root.join(relative);
        if source_path.exists() {
            source_path
        } else {
            self.shared_data_path(relative)
        }
    }

    pub(crate) fn shared_data_path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.shared_data_root.join(relative)
    }

    pub(crate) fn casc_cache_path(&self, product: &str, build_key: &str) -> PathBuf {
        self.cache_root.join("casc").join(product).join(build_key)
    }

    pub(crate) fn remap_to_shared_data_path(&self, path: &Path) -> PathBuf {
        if let Ok(stripped) = path.strip_prefix(&self.source_data_root) {
            return self.shared_data_path(stripped);
        }
        if let Ok(stripped) = path.strip_prefix("data") {
            return self.shared_data_path(stripped);
        }
        path.to_path_buf()
    }
}

pub(crate) fn default_paths() -> &'static ResolverPaths {
    DEFAULT_PATHS.get_or_init(|| ResolverPaths::from_config(AssetResolverConfig::default()))
}

pub fn casc_cache_path(product: &str, build_key: &str) -> PathBuf {
    default_paths().casc_cache_path(product, build_key)
}

fn env_source_data_root() -> Option<PathBuf> {
    std::env::var_os("ASSET_RESOLVER_DATA_DIR").map(PathBuf::from)
}

fn env_shared_data_root() -> Option<PathBuf> {
    std::env::var_os("ASSET_RESOLVER_SHARED_DATA_DIR").map(PathBuf::from)
}

fn env_cache_root() -> Option<PathBuf> {
    std::env::var_os("ASSET_RESOLVER_CACHE_DIR").map(PathBuf::from)
}

fn default_source_data_root() -> PathBuf {
    runtime_data_candidates()
        .into_iter()
        .find(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("data"))
}

fn runtime_data_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("data"));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        candidates.push(parent.join("data"));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data"));
    candidates
}

fn default_cache_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache").into()))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::{AssetResolverConfig, ResolverPaths};
    use std::path::PathBuf;

    #[test]
    fn explicit_config_controls_resolver_locations() {
        let config = AssetResolverConfig::new()
            .with_data_root("/tmp/asset-source-data")
            .with_shared_data_root("/tmp/asset-shared-data")
            .with_cache_root("/tmp/asset-cache");

        let paths = ResolverPaths::from_config(config);

        assert_eq!(
            paths.source_data_root(),
            PathBuf::from("/tmp/asset-source-data")
        );
        assert_eq!(
            paths.shared_data_path("local.sqlite"),
            PathBuf::from("/tmp/asset-shared-data/local.sqlite")
        );
        assert_eq!(
            paths.casc_cache_path("wow", "build"),
            PathBuf::from("/tmp/asset-cache/casc/wow/build")
        );
    }

    #[test]
    fn default_paths_do_not_reference_game_engine_repo() {
        let paths = ResolverPaths::from_config(AssetResolverConfig::default());

        assert!(
            !paths
                .source_data_root()
                .to_string_lossy()
                .contains("game-engine")
        );
        assert!(
            !paths
                .shared_data_root()
                .to_string_lossy()
                .contains("game-engine")
        );
        assert!(!paths.cache_root().to_string_lossy().contains("game-engine"));
    }
}
