//! `initialize` opens local CASC up front: afterwards an extraction pays no init.
//! Needs a local WoW install (discovered as `wow_install_path` does).
#[cfg(feature = "casc")]
use std::time::{Duration, Instant};

use asset_resolver::CascListfileResolver;

/// DBFilesClient/SoundKit.db2 (FDID 1237434), present in every retail build.
#[cfg(feature = "casc")]
const SOUND_KIT_DB2: u32 = 1237434;

#[cfg(feature = "casc")]
#[test]
fn initialize_opens_casc_once_and_extraction_then_needs_no_init() {
    let resolver = CascListfileResolver::default();
    resolver.initialize().expect("local CASC initializes");
    let again = Instant::now();
    resolver
        .initialize()
        .expect("a second call reports the first outcome");
    assert!(
        again.elapsed() < Duration::from_millis(50),
        "{:?}",
        again.elapsed()
    );

    let extracting = Instant::now();
    let bytes = resolver
        .resolve_bytes(SOUND_KIT_DB2)
        .expect("SoundKit.db2 in local CASC");
    let took = extracting.elapsed();
    assert!(bytes.starts_with(b"WDC"), "not a WDC database");
    // The init alone takes seconds; one read after it is far below that.
    assert!(took < Duration::from_millis(500), "{took:?}");
}

#[cfg(not(feature = "casc"))]
#[test]
fn initialize_reports_casc_feature_disabled() {
    assert_eq!(
        CascListfileResolver::default().initialize(),
        Err("asset-resolver was built without the casc feature".into())
    );
}
