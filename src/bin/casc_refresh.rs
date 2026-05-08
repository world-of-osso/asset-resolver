//! Refresh the local CASC FDID resolution cache from the discovered WoW install.

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let install_root = asset_resolver::wow_install_path().ok_or_else(|| {
        "WoW install not found (set WOW_INSTALL_PATH or WOW_DATA_PATH, or use a default install path)"
            .to_string()
    })?;
    let cache_dir =
        asset_resolver::casc_resolver::refresh_resolution_cache_for_install(install_root)?;
    eprintln!("Refreshed CASC resolution cache at {}", cache_dir.display());
    Ok(())
}
