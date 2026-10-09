//! Process-wide runtime policy and the last boundary before install/CASC I/O.
use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AssetRuntimeMode {
    #[default]
    LocalCasc,
    ExtractedOnly,
}

impl std::str::FromStr for AssetRuntimeMode {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local-casc" => Ok(Self::LocalCasc),
            "extracted-only" => Ok(Self::ExtractedOnly),
            _ => Err(format!("unknown asset runtime mode: {value}")),
        }
    }
}

static PROCESS_MODE: OnceLock<AssetRuntimeMode> = OnceLock::new();
static FORBIDDEN_ACCESSES: AtomicUsize = AtomicUsize::new(0);
static ACCESS_HOOK: OnceLock<fn(&str)> = OnceLock::new();

/// Call before creating asset readers. An unset environment preserves local CASC.
pub fn configure_runtime_mode_from_env() -> Result<AssetRuntimeMode, String> {
    let mode = match std::env::var("GAME_ENGINE_ASSET_MODE") {
        Ok(value) => value.parse()?,
        Err(std::env::VarError::NotPresent) => AssetRuntimeMode::LocalCasc,
        Err(error) => return Err(format!("GAME_ENGINE_ASSET_MODE: {error}")),
    };
    let selected = *PROCESS_MODE.get_or_init(|| mode);
    if selected != mode {
        return Err("asset runtime mode cannot change after initialization".into());
    }
    Ok(selected)
}

pub(crate) fn process_runtime_mode() -> AssetRuntimeMode {
    *PROCESS_MODE.get_or_init(|| match std::env::var("GAME_ENGINE_ASSET_MODE") {
        Ok(value) => value.parse().expect("invalid GAME_ENGINE_ASSET_MODE"),
        Err(std::env::VarError::NotPresent) => AssetRuntimeMode::LocalCasc,
        Err(error) => panic!("GAME_ENGINE_ASSET_MODE: {error}"),
    })
}

pub(crate) fn effective_runtime_mode(mode: AssetRuntimeMode) -> AssetRuntimeMode {
    if mode == AssetRuntimeMode::ExtractedOnly
        || process_runtime_mode() == AssetRuntimeMode::ExtractedOnly
    {
        AssetRuntimeMode::ExtractedOnly
    } else {
        AssetRuntimeMode::LocalCasc
    }
}

/// Single policy choke point, called before any install discovery or CASC I/O.
/// A denied entry records evidence before invoking the hook or returning an error.
pub fn guard_casc_access(mode: AssetRuntimeMode, operation: &str) -> Result<(), String> {
    if effective_runtime_mode(mode) == AssetRuntimeMode::LocalCasc {
        return Ok(());
    }
    FORBIDDEN_ACCESSES.fetch_add(1, Ordering::SeqCst);
    let error = format!("extracted-only forbids CASC access: {operation}");
    if let Some(hook) = ACCESS_HOOK.get() {
        hook(&error);
    }
    Err(error)
}

pub fn forbidden_casc_access_count() -> usize {
    FORBIDDEN_ACCESSES.load(Ordering::SeqCst)
}

/// Install once in a fresh test process. A panic hook makes swallowed Option/log
/// errors observable; the monotonic counter also catches a caught panic.
pub fn set_casc_access_hook(hook: fn(&str)) -> Result<(), String> {
    ACCESS_HOOK
        .set(hook)
        .map_err(|_| "CASC access hook already installed".into())
}
