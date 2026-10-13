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
        let root_encoded = self.store(&root);
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
