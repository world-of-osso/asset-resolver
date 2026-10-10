//! Product/build isolation exercises real cache IO, not M2 or BLP parsing.
use asset_resolver::{AssetIdentity, AssetResolverConfig, AssetRuntimeMode, CascListfileResolver};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "asset-identity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn resolver(&self, identity: AssetIdentity) -> CascListfileResolver {
        CascListfileResolver::new(
            AssetResolverConfig::new()
                .with_data_root(&self.0)
                .with_shared_data_root(&self.0)
                .with_cache_root(self.0.join("cache"))
                .with_identity(identity),
        )
    }
    fn seed(&self, identity: &AssetIdentity, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = identity.asset_path(&self.0, relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

const RETAIL_KEY: &str = "dcfc90fffd79ba00406ae46f5f657592";
const FOREVER_KEY: &str = "842b2e5d11f8d6fe257a5b73bd5cf6c6";

#[test]
fn same_fdids_and_full_companion_chain_coexist_in_one_process() {
    let fixture = Fixture::new();
    let retail = AssetIdentity::new("wow", RETAIL_KEY).unwrap();
    let forever = AssetIdentity::new("wow_classic_beta", FOREVER_KEY).unwrap();
    let retail_resolver = fixture.resolver(retail.clone());
    let forever_resolver = fixture.resolver(forever.clone());
    for relative in [
        "models/1100087.m2",
        "models/110008700.skin",
        "models/1100087.skel",
        "models/1100258.anim",
        "textures/7731197.blp",
    ] {
        let retail_path = fixture.seed(&retail, relative, b"retail-authored");
        let forever_path = fixture.seed(&forever, relative, b"forever-authored");
        let unqualified = fixture.0.join(relative);
        fs::create_dir_all(unqualified.parent().unwrap()).unwrap();
        fs::write(&unqualified, b"unqualified-decoy").unwrap();
        for _ in 0..2 {
            let selected = retail_resolver
                .ensure_cached(1100087, &unqualified)
                .unwrap()
                .unwrap();
            assert_eq!(selected, retail_path);
            assert_eq!(fs::read(selected).unwrap(), b"retail-authored");
            let selected = forever_resolver
                .ensure_cached(1100087, &unqualified)
                .unwrap()
                .unwrap();
            assert_eq!(selected, forever_path);
            assert_eq!(fs::read(selected).unwrap(), b"forever-authored");
        }
        assert_eq!(fs::read(unqualified).unwrap(), b"unqualified-decoy");
    }
}

#[test]
fn builds_of_same_product_do_not_share_cached_bytes() {
    let fixture = Fixture::new();
    let first = AssetIdentity::new("wow_classic_beta", FOREVER_KEY).unwrap();
    let second =
        AssetIdentity::new("wow_classic_beta", "e8dd824cf6c3d96cd01f804ca2ea5a63").unwrap();
    let relative = "models/1100258.m2";
    fixture.seed(&first, relative, b"70205");
    fixture.seed(&second, relative, b"70291");
    for (identity, expected) in [(first, b"70205"), (second, b"70291")] {
        let path = fixture
            .resolver(identity)
            .ensure_cached(1100258, &fixture.0.join(relative))
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), expected);
    }
}

#[test]
fn unavailable_pinned_build_cannot_borrow_legacy_or_current_build() {
    let fixture = Fixture::new();
    let identity =
        AssetIdentity::new("wow_classic_beta", "00000000000000000000000000000000").unwrap();
    let destination = fixture.0.join("models/1100087.m2");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, b"legacy-retail").unwrap();
    let resolver = fixture.resolver(identity);
    let error = resolver
        .ensure_cached_checked(1100087, &destination)
        .unwrap_err();
    assert!(
        error.contains("00000000000000000000000000000000"),
        "{error}"
    );
    assert_eq!(fs::read(destination).unwrap(), b"legacy-retail");
}

#[test]
fn qualified_paths_keep_their_identity_and_cannot_be_relabelled() {
    let fixture = Fixture::new();
    let retail = AssetIdentity::new("wow", RETAIL_KEY).unwrap();
    let forever = AssetIdentity::new("wow_classic_beta", FOREVER_KEY).unwrap();
    let retail_path = fixture.seed(&retail, "models/1100087.m2", b"retail-authored");
    let forever_path = fixture.seed(&forever, "models/1100087.m2", b"forever-authored");
    let resolver = fixture.resolver(forever);
    assert_eq!(
        resolver
            .ensure_cached_checked(1100087, &forever_path)
            .unwrap(),
        forever_path
    );
    let error = resolver
        .ensure_cached_checked(1100087, &retail_path)
        .unwrap_err();
    assert!(error.contains("does not belong"), "{error}");
    assert_eq!(fs::read(retail_path).unwrap(), b"retail-authored");
}

#[test]
fn extracted_only_initializes_without_local_casc_and_reads_matching_bytes() {
    let fixture = Fixture::new();
    let identity =
        AssetIdentity::new("wow_classic_beta", "00000000000000000000000000000000").unwrap();
    let path = fixture.seed(&identity, "models/1100087.m2", b"shipped-forever");
    let resolver = CascListfileResolver::new(
        AssetResolverConfig::new()
            .with_data_root(&fixture.0)
            .with_shared_data_root(&fixture.0)
            .with_cache_root(fixture.0.join("cache"))
            .with_identity(identity)
            .with_runtime_mode(AssetRuntimeMode::ExtractedOnly),
    );
    assert_eq!(resolver.runtime_mode(), AssetRuntimeMode::ExtractedOnly);
    resolver
        .initialize()
        .expect("extracted-only must not open an unavailable local CASC build");
    let selected = resolver
        .ensure_cached_checked(1100087, &fixture.0.join("models/1100087.m2"))
        .unwrap();
    assert_eq!(selected, path);
    assert_eq!(
        resolver
            .ensure_cached(1100087, &fixture.0.join("models/1100087.m2"))
            .unwrap(),
        Some(path)
    );
    assert_eq!(fs::read(selected).unwrap(), b"shipped-forever");
}

#[test]
fn extracted_only_missing_asset_does_not_attempt_local_casc_or_legacy_bytes() {
    let fixture = Fixture::new();
    let identity =
        AssetIdentity::new("wow_classic_beta", "00000000000000000000000000000000").unwrap();
    let destination = fixture.0.join("models/1100087.m2");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, b"legacy").unwrap();
    let resolver = CascListfileResolver::new(
        AssetResolverConfig::new()
            .with_data_root(&fixture.0)
            .with_shared_data_root(&fixture.0)
            .with_cache_root(fixture.0.join("cache"))
            .with_identity(identity)
            .with_runtime_mode(AssetRuntimeMode::ExtractedOnly),
    );
    let error = resolver
        .ensure_cached_checked(1100087, &destination)
        .unwrap_err();
    assert_eq!(
        resolver.ensure_cached(1100087, &destination).unwrap_err(),
        error
    );
    assert!(error.contains("extracted-only"), "{error}");
    assert!(
        error.contains("00000000000000000000000000000000"),
        "{error}"
    );
    assert_eq!(fs::read(destination).unwrap(), b"legacy");
    assert!(
        !fixture.0.join("cache").exists(),
        "runtime must not create a CASC cache"
    );
}

#[test]
fn authored_identity_rejects_invalid_product_or_build_key() {
    assert!(AssetIdentity::new("../wow", RETAIL_KEY).is_err());
    assert!(AssetIdentity::new("wow", "active").is_err());
    assert!(AssetIdentity::new("wow", "../dcfc90fffd79ba00406ae46f5f657592").is_err());
}
