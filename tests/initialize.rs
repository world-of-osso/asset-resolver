//! `initialize` opens local CASC up front: afterwards an extraction pays no init.
//! Needs a local WoW install (discovered as `wow_install_path` does).
#[cfg(feature = "casc")]
use std::time::Instant;

use asset_resolver::CascListfileResolver;

/// DBFilesClient/SoundKit.db2 (FDID 1237434), present in every retail build.
#[cfg(feature = "casc")]
const SOUND_KIT_DB2: u32 = 1237434;

#[cfg(feature = "casc")]
#[test]
fn initialize_is_reusable_across_resolvers_and_reads_local_db2() {
    std::thread::spawn(|| CascListfileResolver::default().initialize())
        .join()
        .expect("startup worker finishes")
        .expect("local CASC initializes on worker");
    let resolver = CascListfileResolver::default();
    let again = Instant::now();
    resolver
        .initialize()
        .expect("a second call reports the first outcome");
    eprintln!("repeated initialization: {:?}", again.elapsed());

    let extracting = Instant::now();
    let bytes = resolver
        .resolve_bytes(SOUND_KIT_DB2)
        .expect("SoundKit.db2 in local CASC");
    let took = extracting.elapsed();
    assert!(bytes.starts_with(b"WDC"), "not a WDC database");
    eprintln!("first initialized SoundKit read: {took:?}");
    let other = CascListfileResolver::default();
    other
        .initialize()
        .expect("another resolver reuses CASC state");
    assert_eq!(other.resolve_bytes(SOUND_KIT_DB2), Some(bytes));
}

#[cfg(not(feature = "casc"))]
#[test]
fn initialize_reports_casc_feature_disabled() {
    assert_eq!(
        CascListfileResolver::default().initialize(),
        Err("asset-resolver was built without the casc feature".into())
    );
}
