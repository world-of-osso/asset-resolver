//! Internal CASC-backed extractor for the disk asset cache.
//!
//! Reads directly from a local WoW installation discovered via
//! [`wow_install_path`]. On first use, parses `.build.info` and the build
//! config to find root/encoding keys, loads cached resolution files, and
//! lazily initializes archive indices only when an actual FDID extraction is
//! needed.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use binrw::BinRead;
use cascette_client_storage::index::IndexManager;
use cascette_client_storage::storage::ArchiveManager;
use cascette_client_storage::{BuildInfoFile, Installation};

use crate::casc_cache::CascResolutionCache;
use crate::listfile::Listfile;
use crate::paths::ResolverPaths;
use cascette_crypto::{ContentKey, EncodingKey, TactKeyStore};
use cascette_formats::blte::BlteFile;
use cascette_formats::config::BuildConfig;
use cascette_formats::encoding::EncodingFile;
use tokio::runtime::Handle as TokioHandle;

const LOCAL_CASC_HEADER_SIZE: usize = 30;
const EXTERNAL_TACT_KEYS_PATH: &str = "tactkeys/WoW.txt";
const DEFAULT_WOW_PRODUCT: &str = "wow";

static CASC: OnceLock<Option<CascState>> = OnceLock::new();
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
    paths: ResolverPaths,
    install: Installation,
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
    keys: TactKeyStore,
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
    ) -> Result<Vec<u8>, String> {
        match run_async(self.install.read_file_by_encoding_key(encoding_key)) {
            Ok(data) => Ok(data),
            Err(primary_err) => self
                .read_file_by_encoding_key_with_keys(encoding_key)
                .map_err(|fallback_err| {
                    format!(
                        "{primary_err}; key-aware local archive fallback also failed: {fallback_err}"
                    )
                }),
        }
    }

    fn read_file_by_path(&self, path: &str) -> Result<Vec<u8>, String> {
        run_async(self.install.read_file_by_path(path))
            .map_err(|err| format!("read CASC path {path}: {err}"))
    }

    fn read_file_by_encoding_key_with_keys(
        &self,
        encoding_key: &cascette_crypto::EncodingKey,
    ) -> Result<Vec<u8>, String> {
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
        let blte_bytes = if raw_blte.len() >= LOCAL_CASC_HEADER_SIZE + 4
            && &raw_blte[LOCAL_CASC_HEADER_SIZE..LOCAL_CASC_HEADER_SIZE + 4] == b"BLTE"
        {
            &raw_blte[LOCAL_CASC_HEADER_SIZE..]
        } else {
            raw_blte.as_slice()
        };
        let blte = BlteFile::read_options(
            &mut std::io::Cursor::new(blte_bytes),
            binrw::Endian::Big,
            (),
        )
        .map_err(|e| format!("parse BLTE container: {e}"))?;
        blte.decompress_with_keys(&local.keys)
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
        let keys = load_tact_keys(&self.paths);

        let init_result = (|| -> Result<LocalArchiveAccess, String> {
            run_async(indices.load_all()).map_err(|e| format!("load CASC indices: {e}"))?;
            run_async(archives.open_all()).map_err(|e| format!("open CASC archives: {e}"))?;
            Ok(LocalArchiveAccess {
                indices,
                archives,
                keys,
            })
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

pub fn ensure_file_cached_at_path(fdid: u32, out_path: &Path) -> Option<PathBuf> {
    ensure_file_cached_at_path_with_paths(
        crate::paths::default_paths(),
        crate::listfile::get_default(),
        fdid,
        out_path,
    )
}

pub(crate) fn ensure_file_cached_at_path_with_paths(
    paths: &ResolverPaths,
    listfile: &Listfile,
    fdid: u32,
    out_path: &Path,
) -> Option<PathBuf> {
    let shared_path = paths.remap_to_shared_data_path(out_path);
    if shared_path.exists() {
        return Some(shared_path);
    }
    let missing_marker = shared_path.with_extension(format!(
        "{}.missing",
        shared_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
    ));
    if missing_marker.exists() {
        return None;
    }
    eprintln!(
        "asset-cache miss: fdid {fdid} not cached at {}, extracting from local CASC",
        shared_path.display()
    );
    match extract_fdid_to_path_with_paths(paths, listfile, fdid, &shared_path) {
        Ok(path) => Some(path),
        Err(err) => {
            eprintln!(
                "asset-cache extraction failed: fdid {fdid} -> {}: {err}",
                shared_path.display()
            );
            write_missing_marker(&missing_marker);
            None
        }
    }
}

fn write_missing_marker(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, []);
}

pub fn resolve_bytes(fdid: u32) -> Option<Vec<u8>> {
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

    match read_fdid_bytes(casc, listfile, fdid) {
        Ok(data) => Some(data),
        Err(err) => {
            eprintln!("asset-cache byte resolve failed: fdid {fdid}: {err}");
            None
        }
    }
}

pub fn extract_fdid_to_path(fdid: u32, out_path: &Path) -> Result<PathBuf, String> {
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
) -> Result<PathBuf, String> {
    let casc = get_casc(paths)?;
    casc.ensure_initialized()?;

    let data = read_fdid_bytes(casc, listfile, fdid)?;
    write_to_path(out_path, &data)?;
    eprintln!("CASC: extracted FDID {fdid} -> {}", out_path.display());
    Ok(out_path.to_path_buf())
}

fn read_fdid_bytes(casc: &CascState, listfile: &Listfile, fdid: u32) -> Result<Vec<u8>, String> {
    match casc.cache.resolve_fdid(fdid) {
        Some((_, encoding_key_bytes)) => {
            read_fdid_bytes_by_encoding_key(casc, fdid, encoding_key_bytes)
        }
        None => read_fdid_bytes_by_listfile_path(casc, listfile, fdid),
    }
}

fn read_fdid_bytes_by_encoding_key(
    casc: &CascState,
    fdid: u32,
    encoding_key_bytes: [u8; 16],
) -> Result<Vec<u8>, String> {
    let encoding_key = cascette_crypto::EncodingKey::from_bytes(encoding_key_bytes);
    casc.read_file_by_encoding_key(&encoding_key)
        .map_err(|e| format!("CASC read FDID {fdid} via encoding key {encoding_key}: {e}"))
}

fn read_fdid_bytes_by_listfile_path(
    casc: &CascState,
    listfile: &Listfile,
    fdid: u32,
) -> Result<Vec<u8>, String> {
    let path = listfile.lookup_fdid(fdid).ok_or_else(|| {
        format!("CASC resolve FDID {fdid}: missing resolution and listfile entry")
    })?;
    casc.read_file_by_path(path)
        .map_err(|e| format!("CASC read FDID {fdid} via listfile path {path}: {e}"))
}

fn write_to_path(out_path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = out_path
        .parent()
        .ok_or_else(|| format!("missing parent for {}", out_path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    std::fs::write(out_path, data).map_err(|e| format!("write {}: {e}", out_path.display()))?;
    Ok(())
}

fn get_casc(paths: &ResolverPaths) -> Result<&'static CascState, String> {
    CASC.get_or_init(|| match init_casc(paths) {
        Ok(state) => Some(state),
        Err(err) => {
            // Surface init failures instead of swallowing them silently;
            // every later caller would otherwise just see "CASC not available"
            // with no clue why bootstrap failed (missing manifest, bad cache,
            // permission error, etc).
            eprintln!("CASC init failed: {err}");
            None
        }
    })
    .as_ref()
    .ok_or_else(|| "CASC not available".to_string())
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

    let active_build = read_active_build(install_root)?;
    let casc_dir = paths.casc_cache_path(&active_build.product, &active_build.build_key);
    ensure_resolution_cache(paths, install_root, &install, &casc_dir)?;
    let cache = CascResolutionCache::open(&casc_dir)?;

    eprintln!(
        "CASC resolver initialized from {data_root_display} using {} cache {}",
        active_build.product,
        casc_dir.display()
    );
    Ok(CascState {
        paths: paths.clone(),
        install,
        cache,
        initialized: Mutex::new(InitState::Uninitialized),
        local_access: Mutex::new(LocalAccessState::Uninitialized),
    })
}

pub fn casc_cache_dir_for_install(install_root: &Path) -> Result<PathBuf, String> {
    let active_build = read_active_build(install_root)?;
    Ok(crate::paths::casc_cache_path(
        &active_build.product,
        &active_build.build_key,
    ))
}

pub fn open_resolution_cache_for_install(
    install_root: &Path,
) -> Result<CascResolutionCache, String> {
    let data_root = install_root.join("Data");
    let install = Installation::open(data_root).map_err(|e| format!("CASC open: {e}"))?;
    let active_build = read_active_build(install_root)?;
    let casc_dir = crate::paths::casc_cache_path(&active_build.product, &active_build.build_key);
    ensure_resolution_cache(
        crate::paths::default_paths(),
        install_root,
        &install,
        &casc_dir,
    )?;
    CascResolutionCache::open(&casc_dir)
}

pub fn refresh_resolution_cache_for_install(install_root: &Path) -> Result<PathBuf, String> {
    let data_root = install_root.join("Data");
    let install = Installation::open(data_root).map_err(|e| format!("CASC open: {e}"))?;
    let active_build = read_active_build(install_root)?;
    let casc_dir = crate::paths::casc_cache_path(&active_build.product, &active_build.build_key);
    rebuild_resolution_cache(
        crate::paths::default_paths(),
        &install,
        &casc_dir,
        &active_build.config,
    )?;
    Ok(casc_dir)
}

fn ensure_resolution_cache(
    paths: &ResolverPaths,
    install_root: &Path,
    install: &Installation,
    casc_dir: &Path,
) -> Result<(), String> {
    if crate::casc_cache::resolution_cache_is_fresh(casc_dir)? {
        return Ok(());
    }

    std::fs::create_dir_all(casc_dir)
        .map_err(|e| format!("create CASC cache dir {}: {e}", casc_dir.display()))?;
    run_async(install.initialize()).map_err(|e| format!("CASC init for cache bootstrap: {e}"))?;

    let active_build = read_active_build(install_root)?;
    rebuild_resolution_cache(paths, install, casc_dir, &active_build.config)
}

fn rebuild_resolution_cache(
    paths: &ResolverPaths,
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
    let encoding_data = read_refresh_file_by_encoding_key(paths, install, &encoding_key)
        .map_err(|e| format!("read encoding file {encoding_key}: {e}"))?;
    std::fs::write(casc_dir.join("encoding.bin"), &encoding_data)
        .map_err(|e| format!("write {}: {e}", casc_dir.join("encoding.bin").display()))?;

    let root_content_key = build_config
        .root()
        .ok_or_else(|| "active WoW build config has no root entry".to_string())
        .and_then(parse_content_key)?;
    let root_encoding_key = resolve_content_key_from_encoding(&encoding_data, &root_content_key)?;
    let root_data = read_refresh_file_by_encoding_key(paths, install, &root_encoding_key)
        .map_err(|e| format!("read root file {root_encoding_key}: {e}"))?;
    std::fs::write(casc_dir.join("root.bin"), &root_data)
        .map_err(|e| format!("write {}: {e}", casc_dir.join("root.bin").display()))?;

    crate::casc_cache::build_resolution_cache(casc_dir)
}

fn read_refresh_file_by_encoding_key(
    paths: &ResolverPaths,
    install: &Installation,
    encoding_key: &EncodingKey,
) -> Result<Vec<u8>, String> {
    match run_async(install.read_file_by_encoding_key(encoding_key)) {
        Ok(data) => Ok(data),
        Err(primary_err) => {
            read_refresh_file_by_local_archive(paths, encoding_key).map_err(|fallback_err| {
                format!(
                    "{primary_err}; key-aware local archive fallback also failed: {fallback_err}"
                )
            })
        }
    }
}

fn read_refresh_file_by_local_archive(
    paths: &ResolverPaths,
    encoding_key: &EncodingKey,
) -> Result<Vec<u8>, String> {
    let data_dir = wow_install_path()
        .ok_or_else(|| "WoW install not found for local archive access".to_string())?
        .join("Data")
        .join("data");
    let mut indices = IndexManager::new(&data_dir);
    let mut archives = ArchiveManager::new(&data_dir);
    let keys = load_tact_keys(paths);
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
    let blte_bytes = if raw_blte.len() >= LOCAL_CASC_HEADER_SIZE + 4
        && &raw_blte[LOCAL_CASC_HEADER_SIZE..LOCAL_CASC_HEADER_SIZE + 4] == b"BLTE"
    {
        &raw_blte[LOCAL_CASC_HEADER_SIZE..]
    } else {
        raw_blte.as_slice()
    };
    let blte = BlteFile::read_options(
        &mut std::io::Cursor::new(blte_bytes),
        binrw::Endian::Big,
        (),
    )
    .map_err(|e| format!("parse BLTE container: {e}"))?;
    blte.decompress_with_keys(&keys)
        .map_err(|e| format!("decrypt/decompress BLTE container: {e}"))
}

fn read_active_build(install_root: &Path) -> Result<ActiveBuild, String> {
    let build_info_path = install_root.join(".build.info");
    let build_info = std::fs::read_to_string(&build_info_path)
        .map_err(|e| format!("read {}: {e}", build_info_path.display()))?;
    let build_info = BuildInfoFile::parse_str(&build_info)
        .map_err(|e| format!("parse {}: {e}", build_info_path.display()))?;
    let selected_product = selected_wow_product();
    let entry = build_info
        .entries()
        .into_iter()
        .find(|entry| entry.is_active() && entry.product() == Some(selected_product.as_str()))
        .or_else(|| build_info.active_entry())
        .ok_or_else(|| format!("{} has no active build entry", build_info_path.display()))?;
    let product = entry
        .product()
        .unwrap_or(selected_product.as_str())
        .to_string();
    let build_key = entry
        .build_key()
        .ok_or_else(|| "active WoW build entry has no build key".to_string())?;
    let build_config_path = data_config_path(install_root, build_key)?;
    let build_config = std::fs::File::open(&build_config_path)
        .map_err(|e| format!("open {}: {e}", build_config_path.display()))?;
    let config = BuildConfig::parse(build_config)
        .map_err(|e| format!("parse {}: {e}", build_config_path.display()))?;
    Ok(ActiveBuild {
        product,
        build_key: build_key.to_string(),
        config,
    })
}

fn selected_wow_product() -> String {
    std::env::var("WOW_PRODUCT").unwrap_or_else(|_| DEFAULT_WOW_PRODUCT.to_string())
}

fn data_config_path(install_root: &Path, key: &str) -> Result<PathBuf, String> {
    if key.len() < 4 {
        return Err(format!("invalid build config key: {key}"));
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

fn load_tact_keys(paths: &ResolverPaths) -> TactKeyStore {
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
    keys
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
