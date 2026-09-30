//! Real local-CASC regression; no downloads or isolated resolution-table caches.
//! Requires FDID 1244035 to resolve to BLP2 in the installed build and the warmed
//! default ~/.cache/asset-resolver cache. Children override only data destinations.
#![cfg(all(feature = "casc", unix))]

use asset_resolver::{AssetResolverConfig, CascListfileResolver};
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const TEST_NAME: &str = "persisted_negative_cache_does_not_block_available_local_texture";
const STAGE_ENV: &str = "ASSET_RESOLVER_NEGATIVE_CACHE_STAGE";
const ROOT_ENV: &str = "ASSET_RESOLVER_NEGATIVE_CACHE_ROOT";
const TEXTURE_FDID: u32 = 1244035;
const UNAVAILABLE_FDID: u32 = u32::MAX;
const COMMUNITY_LISTFILE: &str =
    "/syncthing/Sync/Projects/world-of-osso/game-engine/data/community-listfile.csv";

struct Fixture(PathBuf);

impl Fixture {
    fn create() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data/diagnostics")
            .join(format!("negative-cache-{}-{stamp}", std::process::id()));
        fs::create_dir_all(root.parent().expect("diagnostics parent"))
            .expect("create diagnostics directory");
        fs::create_dir(&root).expect("create unique owned fixture");
        let fixture = Self(root);
        fs::create_dir_all(fixture.0.join("source/textures")).expect("create private source");
        fs::create_dir_all(fixture.0.join("shared/textures")).expect("create private shared cache");
        assert!(
            Path::new(COMMUNITY_LISTFILE).is_file(),
            "precondition: existing local community listfile at {COMMUNITY_LISTFILE}"
        );
        symlink(
            COMMUNITY_LISTFILE,
            fixture.0.join("source/community-listfile.csv"),
        )
        .expect("link existing listfile read-only; never copy or download it");
        fixture
    }

    fn run_stage(&self, stage: &str) -> Output {
        let output = Command::new(std::env::current_exe().expect("current test binary"))
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(STAGE_ENV, stage)
            .env(ROOT_ENV, &self.0)
            // Use HOME/.cache/asset-resolver, not a caller's alternate cache.
            .env_remove("ASSET_RESOLVER_CACHE_DIR")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("LOCALAPPDATA")
            .output()
            .expect("launch fresh test process");
        assert!(
            output.status.success(),
            "stage {stage} failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!(
                "owned fixture cleanup failed at {}: {error}",
                self.0.display()
            );
            if !std::thread::panicking() {
                panic!("owned fixture cleanup failed");
            }
        }
    }
}

#[test]
fn persisted_negative_cache_does_not_block_available_local_texture() {
    if let Some(stage) = std::env::var_os(STAGE_ENV) {
        let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("child fixture root"));
        run_child_stage(stage.to_str().expect("UTF-8 stage"), &root);
        return;
    }

    let fixture = Fixture::create();
    fixture.run_stage("persist");
    let marker = fixture.0.join("shared/textures/1244035.blp.missing");
    assert_eq!(fs::read(&marker).expect("marker survives writer exit"), b"");
    fixture.run_stage("positive");
    let failure = fixture.run_stage("unavailable");
    let stderr = String::from_utf8_lossy(&failure.stderr);
    let failed_path = fixture.0.join("shared/textures/unavailable.blp");
    assert!(
        stderr.lines().any(|line| {
            line.contains(&UNAVAILABLE_FDID.to_string())
                && line.contains(failed_path.to_str().expect("UTF-8 fixture path"))
                && line.contains("missing resolution and listfile entry")
        }),
        "failure must identify FDID, output path and local resolution error: {stderr}"
    );
    fixture.run_stage("recover");
}

fn run_child_stage(stage: &str, root: &Path) {
    let requested = root.join("source/textures/1244035.blp");
    let shared = root.join("shared/textures/1244035.blp");
    let marker = shared.with_extension("blp.missing");
    if stage == "persist" {
        persist_legacy_marker(&shared, &marker);
        return;
    }

    let resolver = CascListfileResolver::new(
        AssetResolverConfig::new()
            .with_data_root(root.join("source"))
            .with_shared_data_root(root.join("shared")),
    );
    match stage {
        "recover" => recover_local_texture(&resolver, &requested, &shared, &marker),
        "positive" => preserve_positive_cache(&resolver, root),
        "unavailable" => assert_unavailable(&resolver, root),
        _ => panic!("unknown child stage: {stage}"),
    }
}

fn persist_legacy_marker(shared: &Path, marker: &Path) {
    assert!(!shared.exists(), "positive cache must start absent");
    fs::write(marker, []).expect("persist historical empty missing marker");
}

fn recover_local_texture(
    resolver: &CascListfileResolver,
    requested: &Path,
    shared: &Path,
    marker: &Path,
) {
    assert_eq!(fs::read(marker).expect("persisted marker"), b"");
    assert!(!requested.exists());
    assert!(!shared.exists());
    let bytes = resolver
        .resolve_bytes(TEXTURE_FDID)
        .expect("precondition: FDID 1244035 is available in actual local CASC");
    assert!(bytes.starts_with(b"BLP2"), "local FDID must be BLP2");
    assert_eq!(
        resolver.ensure_cached(TEXTURE_FDID, requested),
        Some(shared.to_path_buf()),
        "persisted empty marker must not block locally available texture"
    );
    assert_eq!(fs::read(shared).expect("acquired shared texture"), bytes);
    assert!(!requested.exists(), "must not write source output path");
}

fn preserve_positive_cache(resolver: &CascListfileResolver, root: &Path) {
    let requested = root.join("source/textures/positive.blp");
    let shared = root.join("shared/textures/positive.blp");
    let bytes = b"user-owned positive fixture; deliberately not CASC bytes\0\xff";
    fs::write(&shared, bytes).expect("seed positive user fixture");
    let before = fs::metadata(&shared).expect("positive file state");
    assert_eq!(
        resolver.ensure_cached(TEXTURE_FDID, &requested),
        Some(shared.clone())
    );
    let after = fs::metadata(&shared).expect("positive file state after read");
    assert_eq!(fs::read(&shared).expect("positive bytes"), bytes);
    assert_eq!(after.ino(), before.ino(), "must not replace positive file");
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    assert_eq!(
        after.ctime(),
        before.ctime(),
        "must not rewrite positive file"
    );
    assert_eq!(after.ctime_nsec(), before.ctime_nsec());
    assert!(!requested.exists());
}

fn assert_unavailable(resolver: &CascListfileResolver, root: &Path) {
    assert!(resolver.resolve_bytes(UNAVAILABLE_FDID).is_none());
    let requested = root.join("source/textures/unavailable.blp");
    let shared = root.join("shared/textures/unavailable.blp");
    assert_eq!(resolver.ensure_cached(UNAVAILABLE_FDID, &requested), None);
    assert!(!shared.exists(), "failure must not create positive output");
    assert!(!requested.exists(), "failure must not write source output");
}
