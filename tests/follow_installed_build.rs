//! Exercise an actual local archive, cache bootstrap, patch and extraction in one process.
#![cfg(feature = "casc")]
use asset_resolver::{AssetIdentity, AssetResolverConfig, CascListfileResolver};
use cascette_client_storage::index::IndexManager;
use cascette_crypto::{ContentKey, EncodingKey, md5::FileDataId};
use cascette_formats::{
    encoding::{CKeyEntryData, EKeyEntryData, EncodingBuilder},
    root::{ContentFlags, LocaleFlags, RootBuilder, RootVersion},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Install {
    root: PathBuf,
    archive: Vec<u8>,
    indices: IndexManager,
    root_span: std::ops::Range<usize>,
}
impl Install {
    fn store(&mut self, raw: &[u8]) -> EncodingKey {
        let mut blte = b"BLTE\0\0\0\0N".to_vec();
        blte.extend(raw);
        let key = EncodingKey::from_data(&blte);
        self.indices
            .add_entry(&key, 0, self.archive.len() as u32, blte.len() as u32)
            .unwrap();
        self.archive.extend(blte);
        key
    }
    fn build(&mut self, raw: &[u8]) -> String {
        let content = ContentKey::from_data(raw);
        let encoded = self.store(raw);
        let mut root = RootBuilder::new(RootVersion::V1);
        root.add_file(
            FileDataId::new(120191),
            content,
            Some("fixture.blp"),
            LocaleFlags::new(LocaleFlags::ENUS),
            ContentFlags::new(0),
        );
        let root = root.build().unwrap();
        let root_content = ContentKey::from_data(&root);
        let root_start = self.archive.len();
        let root_encoded = self.store(&root);
        self.root_span = root_start..self.archive.len();
        let mut encoding = EncodingBuilder::new();
        for (content_key, encoding_key, size) in [
            (content, encoded, raw.len()),
            (root_content, root_encoded, root.len()),
        ] {
            encoding.add_ckey_entry(CKeyEntryData {
                content_key,
                file_size: size as u64,
                encoding_keys: vec![encoding_key],
            });
            encoding.add_ekey_entry(EKeyEntryData {
                encoding_key,
                espec: "n".to_owned(),
                file_size: size as u64 + 9,
            });
        }
        let encoding = encoding.build().unwrap().build().unwrap();
        let encoding_content = ContentKey::from_data(&encoding);
        let encoding_key = self.store(&encoding);
        let config =
            format!("root = {root_content}\nencoding = {encoding_content} {encoding_key}\n");
        let key = ContentKey::from_data(config.as_bytes()).to_hex();
        let path = self
            .root
            .join(format!("Data/config/{}/{}/{key}", &key[..2], &key[2..4]));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, config).unwrap();
        key
    }
    fn activate(&self, key: &str) {
        fs::write(
            self.root.join(".build.info"),
            format!("Active!DEC:1|Build Key!HEX:16|Product!STRING:0\n1|{key}|wow\n"),
        )
        .unwrap();
    }
}

#[test]
fn patch_changes_extracted_bytes_and_cache_namespace_without_relabelling_receipts() {
    let root = std::env::temp_dir().join(format!("casc-follow-install-{}", std::process::id()));
    fs::create_dir_all(root.join("install/Data/data")).unwrap();
    let install_root = root.join("install");
    let mut install = Install {
        root: install_root.clone(),
        archive: vec![],
        indices: IndexManager::new(install_root.join("Data/data")),
        root_span: 0..0,
    };
    let first = install.build(b"BLP2-before-patch");
    let second = install.build(b"BLP2-after-patch");
    install.indices.save_all().unwrap();
    fs::write(install_root.join("Data/data/data.000"), &install.archive).unwrap();
    install.activate(&first);
    unsafe {
        std::env::set_var("WOW_INSTALL_PATH", &install_root);
    }
    let data = root.join("data");
    fs::create_dir_all(data.join("casc")).unwrap();
    // Legacy unqualified tables are not evidence of the currently installed build.
    fs::write(data.join("casc/root.bin"), b"stale legacy tables").unwrap();
    let resolver = CascListfileResolver::new(
        AssetResolverConfig::new()
            .with_data_root(&data)
            .with_shared_data_root(&data)
            .with_cache_root(root.join("cache")),
    );
    let before = root.join("before.blp");
    assert_eq!(
        resolver.resolve_bytes(120191).unwrap(),
        b"BLP2-before-patch"
    );
    fs::write(&before, b"BLP2-before-patch").unwrap();
    install.activate(&second);
    assert_eq!(resolver.resolve_bytes(120191).unwrap(), b"BLP2-after-patch");
    for key in [&first, &second] {
        assert!(
            root.join(format!("cache/casc/wow/{key}/schema-2/resolution.sqlite"))
                .is_file()
        );
    }
    let historical = AssetIdentity::new("wow", &first).unwrap();
    let published = historical.asset_path(&data, Path::new("textures/42.blp"));
    fs::create_dir_all(published.parent().unwrap()).unwrap();
    fs::write(&published, b"historical-receipt-bytes").unwrap();
    let authored = CascListfileResolver::new(
        AssetResolverConfig::new()
            .with_data_root(&data)
            .with_shared_data_root(&data)
            .with_cache_root(root.join("cache"))
            .with_identity(historical),
    );
    assert_eq!(
        authored
            .ensure_cached_checked(42, &data.join("textures/42.blp"))
            .unwrap(),
        published
    );
    fs::create_dir_all(data.join("textures")).unwrap();
    fs::write(data.join("textures/120191.blp"), b"unqualified-decoy").unwrap();
    let extracted = authored
        .ensure_cached_checked(120191, &data.join("textures/120191.blp"))
        .unwrap();
    assert_eq!(fs::read(&extracted).unwrap(), b"BLP2-after-patch");
    assert_eq!(
        extracted,
        data.join(format!("products/wow/{second}/textures/120191.blp"))
    );
    assert_eq!(fs::read(published).unwrap(), b"historical-receipt-bytes");
    assert_eq!(
        fs::read(data.join("textures/120191.blp")).unwrap(),
        b"unqualified-decoy"
    );
    fs::remove_dir_all(root).unwrap();
}

fn cache_fixture(name: &str) -> (PathBuf, Install, String) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("casc-{name}-{}-{nonce}", std::process::id()));
    let install_root = root.join("install");
    fs::create_dir_all(install_root.join("Data/data")).unwrap();
    let mut install = Install {
        root: install_root.clone(),
        archive: vec![],
        indices: IndexManager::new(install_root.join("Data/data")),
        root_span: 0..0,
    };
    let key = install.build(b"BLP2-concurrent-readers");
    install.indices.save_all().unwrap();
    fs::write(install.root.join("Data/data/data.000"), &install.archive).unwrap();
    install.activate(&key);
    (root, install, key)
}

fn spawn_cache_reader(root: &Path) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cache_process_reader", "--nocapture"])
        .env("CASC_PROCESS_FIXTURE", root)
        .env("WOW_INSTALL_PATH", root.join("install"))
        .env("WOW_PRODUCT", "wow")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn cache_process_reader() {
    let Some(root) = std::env::var_os("CASC_PROCESS_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    if std::env::var_os("CASC_INTERRUPT_BUILDER").is_some() {
        let key = fs::read_to_string(root.join("build-key")).unwrap();
        let namespace = root.join(format!("cache/casc/wow/{key}/schema-2"));
        asset_resolver::casc_cache::publish_resolution_namespace(&namespace, |staging| {
            fs::write(
                staging.join("encoding.bin"),
                b"interrupted partial metadata",
            )
            .unwrap();
            fs::write(root.join("builder-started"), b"ready").unwrap();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        })
        .unwrap();
        return;
    }
    let resolver = CascListfileResolver::new(
        AssetResolverConfig::new()
            .with_data_root(root.join("data"))
            .with_shared_data_root(root.join("data"))
            .with_cache_root(root.join("cache")),
    );
    assert_eq!(
        resolver.resolve_bytes(120191).unwrap(),
        b"BLP2-concurrent-readers"
    );
}

#[test]
fn concurrent_processes_publish_one_complete_cache() {
    let (root, _install, key) = cache_fixture("concurrent");
    let children: Vec<_> = (0..12).map(|_| spawn_cache_reader(&root)).collect();
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    let mut builds = 0;
    for output in outputs {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "reader failed: {stderr}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        builds += stderr.matches("Built CASC resolution cache").count();
    }
    assert_eq!(builds, 1, "only the lock owner may build");
    let namespace = root.join(format!("cache/casc/wow/{key}/schema-2"));
    let cache = asset_resolver::casc_cache::CascResolutionCache::open(&namespace).unwrap();
    assert!(cache.resolve_fdid(120191).is_some());
    assert!(namespace.join("root.bin").is_file());
    assert!(namespace.join("encoding.bin").is_file());
    assert!(!namespace.join("resolution.sqlite-journal").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_builder_leaves_no_visible_cache_and_next_process_recovers() {
    let (root, _install, key) = cache_fixture("interrupted");
    fs::write(root.join("build-key"), &key).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cache_process_reader", "--nocapture"])
        .env("CASC_PROCESS_FIXTURE", &root)
        .env("CASC_INTERRUPT_BUILDER", "1")
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !root.join("builder-started").exists() {
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("builder never entered private staging directory");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let namespace = root.join(format!("cache/casc/wow/{key}/schema-2"));
    let visible_while_building = namespace.exists();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        !visible_while_building,
        "in-progress metadata must stay private"
    );
    assert!(!namespace.exists());
    let output = spawn_cache_reader(&root).wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asset_resolver::casc_cache::CascResolutionCache::open(&namespace)
            .unwrap()
            .resolve_fdid(120191)
            .is_some()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn truncated_metadata_archive_does_not_publish_a_namespace() {
    let (root, install, key) = cache_fixture("truncated");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(install.root.join("Data/data/data.000"))
        .unwrap();
    file.set_len(install.archive.len() as u64 - 5).unwrap();
    let output = spawn_cache_reader(&root).wait_with_output().unwrap();
    assert!(
        !output.status.success(),
        "truncated metadata must fail explicitly"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("encoding"),
        "missing metadata error: {stderr}"
    );
    assert!(!root.join(format!("cache/casc/wow/{key}/schema-2")).exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn mid_patch_metadata_does_not_publish_a_namespace() {
    let (root, mut install, key) = cache_fixture("mid-patch");
    // The index still advertises the original root entry, but the patch has
    // overwritten only its payload. Encoding remains readable.
    install.archive[install.root_span.start + 9..install.root_span.end].fill(0);
    fs::write(install.root.join("Data/data/data.000"), &install.archive).unwrap();
    let output = spawn_cache_reader(&root).wait_with_output().unwrap();
    assert!(
        !output.status.success(),
        "mid-patch metadata must fail explicitly"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("root"), "missing metadata error: {stderr}");
    assert!(
        !root.join(format!("cache/casc/wow/{key}/schema-2")).exists(),
        "failed builders must not publish any cache files"
    );
    fs::remove_dir_all(root).unwrap();
}
