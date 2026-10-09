//! Internal CASC-backed extractor for the disk asset cache.
//!
//! Reads directly from a local WoW installation discovered via
//! [`wow_install_path`]. On first use, reads the requested product from
//! `.product.db` and its build config to find root/encoding keys, loads cached
//! resolution files, and lazily initializes archive indices only when an actual FDID extraction is
//! needed.

use crate::AssetIdentity;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use binrw::BinRead;
use cascette_client_storage::index::IndexManager;
use cascette_client_storage::storage::ArchiveManager;
use cascette_client_storage::{BuildInfoFile, Installation, read_installed_product};

use crate::casc_cache::CascResolutionCache;
use crate::listfile::Listfile;
use crate::paths::ResolverPaths;
use cascette_crypto::{ContentKey, EncodingKey, TactKeyStore};
use cascette_formats::blte::{BlteFile, PartialDecode};
use cascette_formats::config::{BuildConfig, KeyringConfig};
use cascette_formats::encoding::EncodingFile;
use tokio::runtime::Handle as TokioHandle;

const LOCAL_CASC_HEADER_SIZE: usize = 30;
const EXTERNAL_TACT_KEYS_PATH: &str = "tactkeys/WoW.txt";
const DEFAULT_WOW_PRODUCT: &str = "wow";

pub use cascette_formats::blte::MissingKeyChunk;

/// A file extracted from local CASC.
#[derive(Debug)]
pub struct ExtractedFile {
    pub path: PathBuf,
    /// Encrypted chunks whose TACT key is unknown; their ranges are zero-filled.
    pub missing_keys: Vec<MissingKeyChunk>,
}

/// File content read from local CASC, possibly with zero-filled encrypted chunks.
struct FileContent {
    data: Vec<u8>,
    missing_keys: Vec<MissingKeyChunk>,
}

impl From<PartialDecode> for FileContent {
    fn from(decoded: PartialDecode) -> Self {
        Self {
            data: decoded.data,
            missing_keys: decoded.missing_keys,
        }
    }
}

type CascCell = Arc<OnceLock<Result<Arc<CascState>, String>>>;
type CascNamespace = (PathBuf, Option<AssetIdentity>);
static CASC: LazyLock<Mutex<HashMap<CascNamespace, CascCell>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static WOW_INSTALL_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Returns the discovered WoW install root (the directory that contains
/// `Data/`, `_retail_/`, etc.), or `None` if no install was found.
///
/// Discovery order:
/// 1. `WOW_INSTALL_PATH` env var (install root containing `Data/`)
/// 2. `WOW_DATA_PATH` env var (full path to the `Data/` dir; parent is the root)
/// 3. A built-in list of common locations (Linux/Wine/Lutris/WSL/macOS).
///
/// A candidate is accepted only if `<root>/Data/data` exists (the directory
/// holding `.idx`/archive blobs), to avoid matching a non-WoW directory.
pub fn wow_install_path() -> Option<&'static Path> {
    crate::guard_casc_access(
        crate::runtime_mode::process_runtime_mode(),
        "WoW install discovery",
    )
    .ok()?;
    WOW_INSTALL_PATH
        .get_or_init(discover_wow_install_path)
        .as_deref()
}

/// Returns the discovered `Data/` directory inside the WoW install, or
/// `None` if no install was found.
pub fn wow_data_path() -> Option<PathBuf> {
    wow_install_path().map(|root| root.join("Data"))
}

fn discover_wow_install_path() -> Option<PathBuf> {
    if let Ok(install) = std::env::var("WOW_INSTALL_PATH") {
        let root = PathBuf::from(install);
        if is_valid_wow_install(&root) {
            return Some(root);
        }
    }
    if let Ok(data) = std::env::var("WOW_DATA_PATH") {
        let data_path = PathBuf::from(data);
        if let Some(root) = data_path.parent() {
            if is_valid_wow_install(root) {
                return Some(root.to_path_buf());
            }
        }
    }
    for candidate in candidate_install_paths() {
        if is_valid_wow_install(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_valid_wow_install(root: &Path) -> bool {
    root.join("Data").join("data").is_dir()
}

fn candidate_install_paths() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("/syncthing/World of Warcraft"),
        PathBuf::from("/mnt/c/Program Files (x86)/World of Warcraft"),
        PathBuf::from("/mnt/c/World of Warcraft"),
        PathBuf::from("/Applications/World of Warcraft"),
    ];
    if cfg!(windows) {
        // Cover the common drive letters users put games on. C: is the
        // default Windows install drive; D:-G: are typical secondary
        // drives. We don't enumerate the whole alphabet because the
        // network/removable drives would be probed too.
        for letter in ['C', 'D', 'E', 'F', 'G'] {
            let drive = format!("{letter}:\\");
            candidates.extend([
                PathBuf::from(format!("{drive}World of Warcraft")),
                PathBuf::from(format!("{drive}Program Files (x86)\\World of Warcraft")),
                PathBuf::from(format!("{drive}Program Files\\World of Warcraft")),
            ]);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.extend([
            home.join("Games/world-of-warcraft/drive_c/Program Files (x86)/World of Warcraft"),
            home.join(".wine/drive_c/Program Files (x86)/World of Warcraft"),
            home.join(".wine/drive_c/World of Warcraft"),
            home.join(".local/share/lutris/runners/wine/World of Warcraft"),
        ]);
    }
    candidates
}

struct CascState {
    identity: Option<AssetIdentity>,
    install: Installation,
    keys: TactKeyStore,
    cache: CascResolutionCache,
    initialized: Mutex<InitState>,
    local_access: Mutex<LocalAccessState>,
}

enum InitState {
    Uninitialized,
    Initialized,
    Failed(String),
}

enum LocalAccessState {
    Uninitialized,
    Initialized(LocalArchiveAccess),
    Failed(String),
}

struct LocalArchiveAccess {
    indices: IndexManager,
    archives: ArchiveManager,
}

struct ActiveBuild {
    product: String,
    build_key: String,
    config: BuildConfig,
}

impl CascState {
    fn ensure_initialized(&self) -> Result<(), String> {
        let mut init = self.initialized.lock().unwrap();
        match &*init {
            InitState::Initialized => return Ok(()),
            InitState::Failed(err) => return Err(err.clone()),
            InitState::Uninitialized => {}
        }
        match run_async(self.install.initialize()).map_err(|e| format!("CASC init: {e}")) {
            Ok(()) => {
                *init = InitState::Initialized;
                Ok(())
            }
            Err(err) => {
                *init = InitState::Failed(err.clone());
                Err(err)
            }
        }
    }

    fn read_file_by_encoding_key(
        &self,
        encoding_key: &cascette_crypto::EncodingKey,
    ) -> Result<FileContent, String> {
        match run_async(self.install.read_file_by_encoding_key(encoding_key)) {
            Ok(data) => Ok(FileContent {
                data,
                missing_keys: Vec::new(),
            }),
            Err(primary_err) => self
                .read_file_by_encoding_key_with_keys(encoding_key)
                .map_err(|fallback_err| {
                    format!(
                        "{primary_err}; key-aware local archive fallback also failed: {fallback_err}"
                    )
                }),
        }
    }

    fn read_file_by_path(&self, path: &str) -> Result<FileContent, String> {
        run_async(self.install.read_file_by_path(path))
            .map(|data| FileContent {
                data,
                missing_keys: Vec::new(),
            })
            .map_err(|err| format!("read CASC path {path}: {err}"))
    }

    fn read_file_by_encoding_key_with_keys(
        &self,
        encoding_key: &cascette_crypto::EncodingKey,
    ) -> Result<FileContent, String> {
        let local = self.ensure_local_access()?;
        let LocalAccessState::Initialized(local) = &*local else {
            return Err("local CASC access not initialized".to_string());
        };
        let index_entry = local
            .indices
            .lookup(encoding_key)
            .ok_or_else(|| format!("missing archive location for encoding key {encoding_key}"))?;
        let raw_blte = local
            .archives
            .read_raw(
                index_entry.archive_id(),
                index_entry.archive_offset(),
                index_entry.size,
            )
            .map_err(|e| format!("read raw BLTE archive entry: {e}"))?;
        parse_local_blte(&raw_blte)?
            .decompress_zeroing_missing_keys(&self.keys)
            .map(FileContent::from)
            .map_err(|e| format!("decrypt/decompress BLTE container: {e}"))
    }

    fn ensure_local_access(&self) -> Result<std::sync::MutexGuard<'_, LocalAccessState>, String> {
        let mut local_access = self.local_access.lock().unwrap();
        match &*local_access {
            LocalAccessState::Initialized(_) => return Ok(local_access),
            LocalAccessState::Failed(err) => return Err(err.clone()),
            LocalAccessState::Uninitialized => {}
        }

        let data_dir = match wow_install_path() {
            Some(root) => root.join("Data").join("data"),
            None => return Err("WoW install not found for local archive access".to_string()),
        };
        let mut indices = IndexManager::new(&data_dir);
        let mut archives = ArchiveManager::new(&data_dir);

        let init_result = (|| -> Result<LocalArchiveAccess, String> {
            run_async(indices.load_all()).map_err(|e| format!("load CASC indices: {e}"))?;
            run_async(archives.open_all()).map_err(|e| format!("open CASC archives: {e}"))?;
            Ok(LocalArchiveAccess { indices, archives })
        })();

        match init_result {
            Ok(access) => {
                *local_access = LocalAccessState::Initialized(access);
                Ok(local_access)
            }
            Err(err) => {
                *local_access = LocalAccessState::Failed(err.clone());
                Err(err)
            }
        }
    }
}

/// Open local CASC (TACT keys, resolution cache) and initialize the installation, as
/// the first extraction would; later calls return the first call's outcome.
pub(crate) fn initialize_with_paths(paths: &ResolverPaths) -> Result<(), String> {
    if paths.runtime_mode() == crate::AssetRuntimeMode::ExtractedOnly {
        return Ok(());
    }
    let casc = get_casc(paths)?;
    casc.ensure_initialized()?;
    // Key-aware reads use their own archive index/access state. Warm it too so
    // encrypted assets do not defer a second initialization to their first read.
    // Only those reads need it: its failure is reported here and again by each of
    // them, and does not fail the initialization every other read relies on.
    if let Err(err) = casc.ensure_local_access() {
        eprintln!("CASC key-aware archive access unavailable: {err}");
    }
    Ok(())
}

pub fn ensure_file_cached_at_path(fdid: u32, out_path: &Path) -> Option<PathBuf> {
    crate::CascListfileResolver::default().ensure_cached(fdid, out_path)
}

pub(crate) fn ensure_file_cached_checked_with_paths(
    paths: &ResolverPaths,
    listfile: &Listfile,
    fdid: u32,
    out_path: &Path,
) -> Result<PathBuf, String> {
    let shared_path = paths.scoped_cache_path(out_path)?;
    if shared_path.is_file() {
        return Ok(shared_path);
    }
    if paths.runtime_mode() == crate::AssetRuntimeMode::ExtractedOnly {
        return Err(paths.missing_extracted_asset(fdid, &shared_path));
    }
    eprintln!(
        "asset-cache miss: fdid {fdid} not cached at {}, extracting from local CASC",
        shared_path.display()
    );
    extract_fdid_to_path_with_paths(paths, listfile, fdid, &shared_path)
        .map(|extracted| extracted.path)
        .map_err(|error| format!("FDID {fdid} at {}: {error}", shared_path.display()))
}

pub fn resolve_bytes(fdid: u32) -> Option<Vec<u8>> {
    crate::guard_casc_access(
        crate::runtime_mode::process_runtime_mode(),
        "raw CASC byte resolution",
    )
    .ok()?;
    resolve_bytes_with_paths(
        crate::paths::default_paths(),
        crate::listfile::get_default(),
        fdid,
    )
}

pub(crate) fn resolve_bytes_with_paths(
    paths: &ResolverPaths,
    listfile: &Listfile,
    fdid: u32,
) -> Option<Vec<u8>> {
    let casc = match get_casc(paths) {
        Ok(casc) => casc,
        Err(err) => {
            eprintln!("asset-cache byte resolve failed: fdid {fdid}: {err}");
            return None;
        }
    };
    if let Err(err) = casc.ensure_initialized() {
        eprintln!("asset-cache byte resolve failed: fdid {fdid}: {err}");
        return None;
    }

    match read_fdid_bytes(&casc, listfile, fdid) {
        Ok(content) => Some(content.data),
        Err(err) => {
            eprintln!("asset-cache byte resolve failed: fdid {fdid}: {err}");
            None
        }
    }
}

pub fn extract_fdid_to_path(fdid: u32, out_path: &Path) -> Result<ExtractedFile, String> {
    crate::guard_casc_access(
        crate::runtime_mode::process_runtime_mode(),
        "direct CASC extraction",
    )?;
    extract_fdid_to_path_with_paths(
        crate::paths::default_paths(),
        crate::listfile::get_default(),
        fdid,
        out_path,
    )
}

fn extract_fdid_to_path_with_paths(
    paths: &ResolverPaths,
    listfile: &Listfile,
    fdid: u32,
    out_path: &Path,
) -> Result<ExtractedFile, String> {
    let casc = get_casc(paths)?;
    casc.ensure_initialized()?;

    let content = read_fdid_bytes(&casc, listfile, fdid)?;
    if paths.identity().is_some() && !content.missing_keys.is_empty() {
        return Err(format!(
            "FDID {fdid} has undecodable encrypted chunks: {}",
            describe_missing_keys(&content.missing_keys)
        ));
    }
    write_to_path(out_path, &content.data)?;
    eprintln!("CASC: extracted FDID {fdid} -> {}", out_path.display());
    Ok(ExtractedFile {
        path: out_path.to_path_buf(),
        missing_keys: content.missing_keys,
    })
}

fn read_fdid_bytes(
    casc: &CascState,
    listfile: &Listfile,
    fdid: u32,
) -> Result<FileContent, String> {
    let content = match casc.cache.resolve_fdid(fdid) {
        Some((_, encoding_key_bytes)) => {
            read_fdid_bytes_by_encoding_key(casc, fdid, encoding_key_bytes)
        }
        None => match &casc.identity {
            Some(identity) => Err(format!(
                "FDID {fdid} absent from {} build {}",
                identity.product(),
                identity.build_key()
            )),
            None => read_fdid_bytes_by_listfile_path(casc, listfile, fdid),
        },
    }?;
    if !content.missing_keys.is_empty() {
        eprintln!(
            "CASC warning: FDID {fdid} has encrypted chunks with unknown TACT keys, zero-filled: {}",
            describe_missing_keys(&content.missing_keys)
        );
    }
    Ok(content)
}

pub fn describe_missing_keys(missing_keys: &[MissingKeyChunk]) -> String {
    missing_keys
        .iter()
        .map(|chunk| {
            format!(
                "{:016X} (chunk {}, {} bytes at offset {})",
                chunk.key_name, chunk.chunk_index, chunk.size, chunk.offset
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn read_fdid_bytes_by_encoding_key(
    casc: &CascState,
    fdid: u32,
    encoding_key_bytes: [u8; 16],
) -> Result<FileContent, String> {
    let encoding_key = cascette_crypto::EncodingKey::from_bytes(encoding_key_bytes);
    casc.read_file_by_encoding_key(&encoding_key)
        .map_err(|e| format!("CASC read FDID {fdid} via encoding key {encoding_key}: {e}"))
}

fn read_fdid_bytes_by_listfile_path(
    casc: &CascState,
    listfile: &Listfile,
    fdid: u32,
) -> Result<FileContent, String> {
    let path = listfile.lookup_fdid(fdid).ok_or_else(|| {
        format!("CASC resolve FDID {fdid}: missing resolution and listfile entry")
    })?;
    casc.read_file_by_path(path)
        .map_err(|e| format!("CASC read FDID {fdid} via listfile path {path}: {e}"))
}

/// Writes `data` to a temporary sibling and renames it over `out_path`, so a concurrent
/// reader that finds `out_path` present always reads the whole file.
fn write_to_path(out_path: &Path, data: &[u8]) -> Result<(), String> {
    static NEXT_TEMPORARY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = out_path
        .parent()
        .ok_or_else(|| format!("missing parent for {}", out_path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    let name = out_path
        .file_name()
        .ok_or_else(|| format!("missing file name in {}", out_path.display()))?;
    let temporary = parent.join(format!(
        ".{}.{}.{}.partial",
        name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&temporary, data).map_err(|e| format!("write {}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, out_path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!(
            "rename {} -> {}: {e}",
            temporary.display(),
            out_path.display()
        )
    })
}

fn get_casc(paths: &ResolverPaths) -> Result<Arc<CascState>, String> {
    crate::guard_casc_access(paths.runtime_mode(), "open CASC state")?;
    let namespace = (paths.cache_root().to_path_buf(), paths.identity().cloned());
    let cell = {
        let mut states = CASC
            .lock()
            .map_err(|error| format!("CASC state lock: {error}"))?;
        Arc::clone(states.entry(namespace).or_default())
    };
    cell.get_or_init(|| init_casc(paths).map(Arc::new)).clone()
}

fn init_casc(paths: &ResolverPaths) -> Result<CascState, String> {
    let install_root = wow_install_path()
        .ok_or_else(|| "WoW install not found (set WOW_INSTALL_PATH or WOW_DATA_PATH, or place install at one of the default locations)".to_string())?;
    let data_root = install_root.join("Data");
    if !data_root.exists() {
        return Err(format!("WoW data not found at {}", data_root.display()));
    }

    let data_root_display = data_root.display().to_string();
    let install = Installation::open(data_root).map_err(|e| format!("CASC open: {e}"))?;

    let active_build = read_requested_build(paths, install_root)?;
    let keys = load_tact_keys(paths, install_root, &active_build)?;
    let casc_dir = paths.casc_cache_path(&active_build.product, &active_build.build_key);
    ensure_resolution_cache(paths, install_root, &install, &casc_dir, &active_build)?;
    let cache = CascResolutionCache::open(&casc_dir)?;

    eprintln!(
        "CASC resolver initialized from {data_root_display} using {} cache {}",
        active_build.product,
        casc_dir.display()
    );
    Ok(CascState {
        identity: paths.identity().cloned(),
        install,
        keys,
        cache,
        initialized: Mutex::new(InitState::Uninitialized),
        local_access: Mutex::new(LocalAccessState::Uninitialized),
    })
}

pub fn casc_cache_dir_for_install(install_root: &Path) -> Result<PathBuf, String> {
    crate::guard_casc_access(
        crate::runtime_mode::process_runtime_mode(),
        "read install build for cache path",
    )?;
    let active_build = read_active_build(install_root)?;
    Ok(crate::paths::casc_cache_path(
        &active_build.product,
        &active_build.build_key,
    ))
}

pub fn open_resolution_cache_for_install(
    install_root: &Path,
) -> Result<CascResolutionCache, String> {
    crate::guard_casc_access(
        crate::runtime_mode::process_runtime_mode(),
        "open install resolution cache",
    )?;
    let data_root = install_root.join("Data");
    let install = Installation::open(data_root).map_err(|e| format!("CASC open: {e}"))?;
    let active_build = read_active_build(install_root)?;
    let casc_dir = crate::paths::casc_cache_path(&active_build.product, &active_build.build_key);
    ensure_resolution_cache(
        crate::paths::default_paths(),
        install_root,
        &install,
        &casc_dir,
        &active_build,
    )?;
    CascResolutionCache::open(&casc_dir)
}

pub fn refresh_resolution_cache_for_install(install_root: &Path) -> Result<PathBuf, String> {
    crate::guard_casc_access(
        crate::runtime_mode::process_runtime_mode(),
        "refresh install resolution cache",
    )?;
    let data_root = install_root.join("Data");
    let install = Installation::open(data_root).map_err(|e| format!("CASC open: {e}"))?;
    let active_build = read_active_build(install_root)?;
    let keys = load_tact_keys(crate::paths::default_paths(), install_root, &active_build)?;
    let casc_dir = crate::paths::casc_cache_path(&active_build.product, &active_build.build_key);
    rebuild_resolution_cache(&keys, &install, &casc_dir, &active_build.config)?;
    Ok(casc_dir)
}

fn ensure_resolution_cache(
    paths: &ResolverPaths,
    install_root: &Path,
    install: &Installation,
    casc_dir: &Path,
    active_build: &ActiveBuild,
) -> Result<(), String> {
    if crate::casc_cache::resolution_cache_is_fresh(casc_dir)? {
        return Ok(());
    }

    std::fs::create_dir_all(casc_dir)
        .map_err(|e| format!("create CASC cache dir {}: {e}", casc_dir.display()))?;
    run_async(install.initialize()).map_err(|e| format!("CASC init for cache bootstrap: {e}"))?;

    let keys = load_tact_keys(paths, install_root, active_build)?;
    rebuild_resolution_cache(&keys, install, casc_dir, &active_build.config)
}

fn rebuild_resolution_cache(
    keys: &TactKeyStore,
    install: &Installation,
    casc_dir: &Path,
    build_config: &BuildConfig,
) -> Result<(), String> {
    std::fs::create_dir_all(casc_dir)
        .map_err(|e| format!("create CASC cache dir {}: {e}", casc_dir.display()))?;

    let encoding_info = build_config
        .encoding()
        .ok_or_else(|| "active WoW build config has no encoding entry".to_string())?;
    let encoding_key = encoding_info
        .encoding_key
        .as_deref()
        .ok_or_else(|| "active WoW build config encoding entry has no encoding key".to_string())
        .and_then(parse_encoding_key)?;
    let encoding_data = read_refresh_file_by_encoding_key(keys, install, &encoding_key)
        .map_err(|e| format!("read encoding file {encoding_key}: {e}"))?;
    std::fs::write(casc_dir.join("encoding.bin"), &encoding_data)
        .map_err(|e| format!("write {}: {e}", casc_dir.join("encoding.bin").display()))?;

    let root_content_key = build_config
        .root()
        .ok_or_else(|| "active WoW build config has no root entry".to_string())
        .and_then(parse_content_key)?;
    let root_encoding_key = resolve_content_key_from_encoding(&encoding_data, &root_content_key)?;
    let root_data = read_refresh_file_by_encoding_key(keys, install, &root_encoding_key)
        .map_err(|e| format!("read root file {root_encoding_key}: {e}"))?;
    std::fs::write(casc_dir.join("root.bin"), &root_data)
        .map_err(|e| format!("write {}: {e}", casc_dir.join("root.bin").display()))?;

    crate::casc_cache::build_resolution_cache(casc_dir)
}

fn read_refresh_file_by_encoding_key(
    keys: &TactKeyStore,
    install: &Installation,
    encoding_key: &EncodingKey,
) -> Result<Vec<u8>, String> {
    match run_async(install.read_file_by_encoding_key(encoding_key)) {
        Ok(data) => Ok(data),
        Err(primary_err) => {
            read_refresh_file_by_local_archive(keys, encoding_key).map_err(|fallback_err| {
                format!(
                    "{primary_err}; key-aware local archive fallback also failed: {fallback_err}"
                )
            })
        }
    }
}

fn read_refresh_file_by_local_archive(
    keys: &TactKeyStore,
    encoding_key: &EncodingKey,
) -> Result<Vec<u8>, String> {
    let data_dir = wow_install_path()
        .ok_or_else(|| "WoW install not found for local archive access".to_string())?
        .join("Data")
        .join("data");
    let mut indices = IndexManager::new(&data_dir);
    let mut archives = ArchiveManager::new(&data_dir);
    run_async(indices.load_all()).map_err(|e| format!("load CASC indices: {e}"))?;
    run_async(archives.open_all()).map_err(|e| format!("open CASC archives: {e}"))?;

    let index_entry = indices
        .lookup(encoding_key)
        .ok_or_else(|| format!("missing archive location for encoding key {encoding_key}"))?;
    let raw_blte = archives
        .read_raw(
            index_entry.archive_id(),
            index_entry.archive_offset(),
            index_entry.size,
        )
        .map_err(|e| format!("read raw BLTE archive entry: {e}"))?;
    // Root and encoding feed the resolution cache, so they must decode fully.
    parse_local_blte(&raw_blte)?
        .decompress_with_keys(keys)
        .map_err(|e| format!("decrypt/decompress BLTE container: {e}"))
}

fn parse_local_blte(raw_blte: &[u8]) -> Result<BlteFile, String> {
    let blte_bytes = if raw_blte.len() >= LOCAL_CASC_HEADER_SIZE + 4
        && &raw_blte[LOCAL_CASC_HEADER_SIZE..LOCAL_CASC_HEADER_SIZE + 4] == b"BLTE"
    {
        &raw_blte[LOCAL_CASC_HEADER_SIZE..]
    } else {
        raw_blte
    };
    BlteFile::read_options(
        &mut std::io::Cursor::new(blte_bytes),
        binrw::Endian::Big,
        (),
    )
    .map_err(|e| format!("parse BLTE container: {e}"))
}

fn read_requested_build(paths: &ResolverPaths, install_root: &Path) -> Result<ActiveBuild, String> {
    let Some(identity) = paths.identity() else {
        return read_active_build(install_root);
    };
    let config_path = data_config_path(install_root, identity.build_key())?;
    let file = std::fs::File::open(&config_path).map_err(|error| {
        format!(
            "open authored {} build {} config {}: {error}",
            identity.product(),
            identity.build_key(),
            config_path.display()
        )
    })?;
    let config = BuildConfig::parse(file)
        .map_err(|error| format!("parse {}: {error}", config_path.display()))?;
    Ok(ActiveBuild {
        product: identity.product().to_owned(),
        build_key: identity.build_key().to_owned(),
        config,
    })
}

fn read_active_build(install_root: &Path) -> Result<ActiveBuild, String> {
    let installed = read_installed_product(install_root, &selected_wow_product())
        .map_err(|e| format!("read installed WoW product: {e}"))?;
    let build_config_path = data_config_path(install_root, &installed.build_key)?;
    let build_config = std::fs::File::open(&build_config_path)
        .map_err(|e| format!("open {}: {e}", build_config_path.display()))?;
    let config = BuildConfig::parse(build_config)
        .map_err(|e| format!("parse {}: {e}", build_config_path.display()))?;
    Ok(ActiveBuild {
        product: installed.product,
        build_key: installed.build_key,
        config,
    })
}

fn selected_wow_product() -> String {
    std::env::var("WOW_PRODUCT").unwrap_or_else(|_| DEFAULT_WOW_PRODUCT.to_string())
}

fn data_config_path(install_root: &Path, key: &str) -> Result<PathBuf, String> {
    if key.len() < 4 {
        return Err(format!("invalid config key: {key}"));
    }
    Ok(install_root
        .join("Data/config")
        .join(&key[0..2])
        .join(&key[2..4])
        .join(key))
}

fn resolve_content_key_from_encoding(
    encoding_data: &[u8],
    content_key: &ContentKey,
) -> Result<EncodingKey, String> {
    let encoding =
        EncodingFile::parse(encoding_data).map_err(|e| format!("parse encoding.bin: {e}"))?;
    for page in &encoding.ckey_pages {
        for entry in &page.entries {
            if &entry.content_key == content_key
                && let Some(encoding_key) = entry.encoding_keys.first()
            {
                return Ok(*encoding_key);
            }
        }
    }
    Err(format!(
        "encoding.bin does not map root content key {content_key}"
    ))
}

fn parse_content_key(value: &str) -> Result<ContentKey, String> {
    parse_hex_16(value).map(ContentKey::from_bytes)
}

fn parse_encoding_key(value: &str) -> Result<EncodingKey, String> {
    parse_hex_16(value).map(EncodingKey::from_bytes)
}

fn parse_hex_16(value: &str) -> Result<[u8; 16], String> {
    if value.len() != 32 {
        return Err(format!("expected 32 hex characters, got {value}"));
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("invalid hex key {value}: {e}"))?;
    }
    Ok(out)
}

/// TACT keys from the external wowdev/TACTKeys list plus the installed
/// build's keyring config (`.build.info` `KeyRing` for the selected product).
fn load_tact_keys(
    paths: &ResolverPaths,
    install_root: &Path,
    active_build: &ActiveBuild,
) -> Result<TactKeyStore, String> {
    let mut keys = TactKeyStore::new();
    let key_path = paths.resolve_data_path(EXTERNAL_TACT_KEYS_PATH);
    if let Ok(content) = std::fs::read_to_string(&key_path) {
        let loaded = keys.load_from_txt(&content);
        if loaded > 0 {
            eprintln!(
                "CASC: loaded {loaded} external TACT keys from {}",
                key_path.display()
            );
        }
    }
    let keyring_keys =
        read_keyring_keys(install_root, &active_build.product, &active_build.build_key)?;
    if !keyring_keys.is_empty() {
        eprintln!(
            "CASC: loaded {} keyring TACT keys for {} build {}",
            keyring_keys.len(),
            active_build.product,
            active_build.build_key
        );
    }
    for key in keyring_keys {
        keys.add(key);
    }
    Ok(keys)
}

fn read_keyring_keys(
    install_root: &Path,
    product: &str,
    build_key: &str,
) -> Result<Vec<cascette_crypto::TactKey>, String> {
    let build_info_path = install_root.join(".build.info");
    let content = std::fs::read_to_string(&build_info_path)
        .map_err(|e| format!("read {}: {e}", build_info_path.display()))?;
    let build_info = BuildInfoFile::parse_str(&content)
        .map_err(|e| format!("parse {}: {e}", build_info_path.display()))?;
    let entries = build_info.entries();
    let entry = entries
        .iter()
        .find(|entry| entry.product() == Some(product) && entry.build_key() == Some(build_key))
        .ok_or_else(|| {
            format!(
                "{} has no row for {product} build {build_key}",
                build_info_path.display()
            )
        })?;
    let Some(keyring_key) = entry.keyring() else {
        return Ok(Vec::new());
    };
    let keyring_path = data_config_path(install_root, keyring_key)?;
    let keyring_file = std::fs::File::open(&keyring_path)
        .map_err(|e| format!("open keyring {}: {e}", keyring_path.display()))?;
    KeyringConfig::parse(keyring_file)
        .map_err(|e| format!("parse keyring {}: {e}", keyring_path.display()))?
        .tact_keys()
        .map_err(|e| format!("invalid keyring {}: {e}", keyring_path.display()))
}

#[cfg(test)]
mod installed_build_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const RETAIL_KEY: &str = "0123456789abcdef0123456789abcdef";
    const FOREVER_KEY: &str = "3bd89ce2721f7c75e7525dc83741076f";

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "asset-resolver-product-db-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn write_config(&self, key: &str, root: &str) {
            let path = data_config_path(&self.0, key).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("root = {root}\nencoding = {root} {root}\n")).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn with_forever_product<T>(f: impl FnOnce() -> T) -> T {
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _lock = ENV_LOCK.lock().unwrap();
        let old = std::env::var_os("WOW_PRODUCT");
        unsafe { std::env::set_var("WOW_PRODUCT", "wow_forever") };
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match &self.0 {
                    Some(value) => unsafe { std::env::set_var("WOW_PRODUCT", value) },
                    None => unsafe { std::env::remove_var("WOW_PRODUCT") },
                }
            }
        }
        let _restore = Restore(old);
        f()
    }

    // Wire-format fixture independent of cascette's protobuf definitions.
    fn varint(mut value: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        while value >= 128 {
            bytes.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
        bytes
    }

    fn field(tag: usize, value: &[u8]) -> Vec<u8> {
        let mut bytes = varint((tag << 3) | 2);
        bytes.extend(varint(value.len()));
        bytes.extend_from_slice(value);
        bytes
    }

    fn product(code: &str, key: &str) -> Vec<u8> {
        let mut base = field(7, b"1.60.1");
        base.extend(field(14, key.as_bytes()));
        base.extend(field(16, RETAIL_KEY.as_bytes()));
        let mut install = field(2, code.as_bytes());
        install.extend(field(4, &field(1, &base)));
        field(1, &install)
    }

    #[test]
    fn requested_forever_build_is_selected_without_build_info_row() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.0.join(".build.info"),
            format!("Active!DEC:1|Build Key!HEX:16|Product!STRING:0\n1|{RETAIL_KEY}|wow\n"),
        )
        .unwrap();
        let mut db = product("wow", RETAIL_KEY);
        db.extend(product("wow_forever", FOREVER_KEY));
        std::fs::write(fixture.0.join(".product.db"), db).unwrap();
        fixture.write_config(RETAIL_KEY, RETAIL_KEY);
        fixture.write_config(FOREVER_KEY, FOREVER_KEY);

        let selected = with_forever_product(|| read_active_build(&fixture.0)).unwrap();
        assert_eq!(selected.product, "wow_forever");
        assert_eq!(selected.build_key, FOREVER_KEY);
        assert_eq!(selected.config.root(), Some(FOREVER_KEY));
    }

    #[test]
    fn authored_build_reads_pinned_config_even_with_a_different_active_build() {
        let fixture = Fixture::new();
        fixture.write_config(RETAIL_KEY, RETAIL_KEY);
        fixture.write_config(FOREVER_KEY, FOREVER_KEY);
        std::fs::write(
            fixture.0.join(".build.info"),
            format!("Active!DEC:1|Build Key!HEX:16|Product!STRING:0\n1|{RETAIL_KEY}|wow\n"),
        )
        .unwrap();
        let identity = AssetIdentity::new("wow_classic_beta", FOREVER_KEY).unwrap();
        let paths =
            ResolverPaths::from_config(crate::AssetResolverConfig::new().with_identity(identity));
        let selected = read_requested_build(&paths, &fixture.0).unwrap();
        assert_eq!(selected.product, "wow_classic_beta");
        assert_eq!(selected.build_key, FOREVER_KEY);
        assert_eq!(selected.config.root(), Some(FOREVER_KEY));
    }

    #[test]
    fn missing_authored_build_does_not_read_the_active_config() {
        let fixture = Fixture::new();
        fixture.write_config(RETAIL_KEY, RETAIL_KEY);
        std::fs::write(
            fixture.0.join(".build.info"),
            format!("Active!DEC:1|Build Key!HEX:16|Product!STRING:0\n1|{RETAIL_KEY}|wow\n"),
        )
        .unwrap();
        let identity = AssetIdentity::new("wow_classic_beta", FOREVER_KEY).unwrap();
        let paths =
            ResolverPaths::from_config(crate::AssetResolverConfig::new().with_identity(identity));
        let error = read_requested_build(&paths, &fixture.0)
            .err()
            .expect("missing pinned config");
        assert!(error.contains(FOREVER_KEY), "{error}");
    }

    const KEYRING_KEY: &str = "3ca57fe7319a297346440e4d2a03a0cd";
    const CLASSIC_KEY: &str = "7dba9c25479632aebc53be9d187818e3";

    fn write_keyring_install(fixture: &Fixture) {
        std::fs::write(
            fixture.0.join(".build.info"),
            format!(
                "Active!DEC:1|Build Key!HEX:16|KeyRing!HEX:16|Product!STRING:0\n\
                 1|{RETAIL_KEY}|{KEYRING_KEY}|wow\n\
                 1|{CLASSIC_KEY}||wow_classic\n"
            ),
        )
        .unwrap();
        let path = data_config_path(&fixture.0, KEYRING_KEY).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            "key-4eb4869f95f23b53 = c9316739348dcc033aa8112f9a3acf5d\n",
        )
        .unwrap();
    }

    #[test]
    fn selected_build_keyring_is_loaded_as_tact_keys() {
        let fixture = Fixture::new();
        write_keyring_install(&fixture);

        let keys = read_keyring_keys(&fixture.0, "wow", RETAIL_KEY).unwrap();

        assert_eq!(
            keys,
            vec![
                cascette_crypto::TactKey::from_hex(
                    0x533B_F295_9F86_B44E,
                    "c9316739348dcc033aa8112f9a3acf5d"
                )
                .unwrap()
            ]
        );
    }

    #[test]
    fn product_without_keyring_loads_no_keyring_keys() {
        let fixture = Fixture::new();
        write_keyring_install(&fixture);

        let keys = read_keyring_keys(&fixture.0, "wow_classic", CLASSIC_KEY).unwrap();

        assert!(keys.is_empty());
    }

    #[test]
    fn keyring_for_other_build_is_an_error() {
        let fixture = Fixture::new();
        write_keyring_install(&fixture);

        let error = read_keyring_keys(&fixture.0, "wow", CLASSIC_KEY).unwrap_err();

        assert!(error.contains("has no row for wow build"), "{error}");
    }

    #[test]
    fn missing_requested_product_errors_instead_of_using_retail() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.0.join(".build.info"),
            format!("Active!DEC:1|Build Key!HEX:16|Product!STRING:0\n1|{RETAIL_KEY}|wow\n"),
        )
        .unwrap();
        std::fs::write(fixture.0.join(".product.db"), product("wow", RETAIL_KEY)).unwrap();
        fixture.write_config(RETAIL_KEY, RETAIL_KEY);

        let error = with_forever_product(|| read_active_build(&fixture.0))
            .err()
            .unwrap();
        assert!(error.contains("wow_forever"), "{error}");
        assert!(error.contains(".product.db"), "{error}");
    }
}

fn run_async<F: std::future::Future>(fut: F) -> F::Output {
    if let Ok(handle) = TokioHandle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(fut))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime")
            .block_on(fut)
    }
}

#[cfg(test)]
mod cache_write_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    /// Two workers extracting models that share a texture: one writes the cache file while
    /// the other finds it present and decodes it. The reader must only ever see the whole
    /// file, never a prefix (Twilight Highlands doodads failed with BLP header EOF).
    #[test]
    fn concurrent_reader_never_sees_a_partly_written_cache_file() {
        let dir = std::env::temp_dir().join(format!("asset-resolver-write-{}", std::process::id()));
        let data = vec![7_u8; 16 << 20];
        let stop = Arc::new(AtomicBool::new(false));
        let paths: Vec<PathBuf> = (0..8)
            .map(|index| dir.join(format!("textures/{index}.blp")))
            .collect();
        let reader = {
            let (stop, paths) = (Arc::clone(&stop), paths.clone());
            std::thread::spawn(move || {
                let mut partial = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    for path in &paths {
                        if let Ok(bytes) = std::fs::read(path)
                            && bytes.len() != 16 << 20
                        {
                            partial.push(bytes.len());
                        }
                    }
                }
                partial
            })
        };
        for path in &paths {
            write_to_path(path, &data).unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        let partial = reader.join().unwrap();
        // Only the eight cache files remain: no temporary file is left behind.
        let files = std::fs::read_dir(dir.join("textures")).unwrap().count();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(partial, Vec::<usize>::new());
        assert_eq!(files, 8);
    }
}
