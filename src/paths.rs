use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static SHARED_REPO_ROOT: OnceLock<PathBuf> = OnceLock::new();
static SOURCE_DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn source_data_root() -> PathBuf {
    SOURCE_DATA_ROOT
        .get_or_init(|| {
            std::env::var_os("ASSET_RESOLVER_DATA_DIR")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("GAME_ENGINE_DATA_DIR").map(PathBuf::from))
                .unwrap_or_else(default_source_data_root)
        })
        .clone()
}

pub fn shared_repo_root() -> PathBuf {
    SHARED_REPO_ROOT
        .get_or_init(|| {
            std::env::var_os("GAME_ENGINE_SHARED_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(default_repo_root)
        })
        .clone()
}

pub fn shared_data_root() -> PathBuf {
    if let Some(path) = std::env::var_os("GAME_ENGINE_SHARED_DATA_DIR") {
        PathBuf::from(path)
    } else {
        default_shared_data_root()
    }
}

pub fn resolve_data_path(relative: impl AsRef<Path>) -> PathBuf {
    let relative = relative.as_ref();
    let source_path = source_data_root().join(relative);
    if source_path.exists() {
        source_path
    } else {
        shared_data_root().join(relative)
    }
}

pub fn shared_data_path(relative: impl AsRef<Path>) -> PathBuf {
    shared_data_root().join(relative)
}

pub fn remap_to_shared_data_path(path: &Path) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix(source_data_root()) {
        return shared_data_path(stripped);
    }
    if let Ok(stripped) = path.strip_prefix("data") {
        return shared_data_path(stripped);
    }
    path.to_path_buf()
}

fn default_source_data_root() -> PathBuf {
    runtime_data_candidates()
        .into_iter()
        .find(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("data"))
}

fn default_repo_root() -> PathBuf {
    let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sibling_engine = manifest_root
        .parent()
        .map(|parent| parent.join("game-engine"))
        .unwrap_or_else(|| manifest_root.clone());
    if sibling_engine.join("data").exists() {
        sibling_engine
    } else {
        manifest_root
    }
}

fn default_shared_data_root() -> PathBuf {
    let repo_data = shared_repo_root().join("data");
    if repo_data.exists() {
        return repo_data;
    }
    default_cache_root().join("asset-resolver/data")
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
    candidates.push(default_repo_root().join("data"));
    candidates
}

fn default_cache_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache").into()))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}
