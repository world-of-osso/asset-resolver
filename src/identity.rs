//! Authored asset namespace. Build keys come from importer provenance, not the
//! product currently selected by the local installation.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssetIdentity {
    product: String,
    build_key: String,
}

impl AssetIdentity {
    pub fn new(product: &str, build_key: &str) -> Result<Self, String> {
        if !matches!(product, "wow" | "wow_classic_beta") {
            return Err(format!("unsupported authored asset product: {product}"));
        }
        if build_key.len() != 32 || !build_key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("invalid authored asset build key: {build_key}"));
        }
        Ok(Self {
            product: product.to_owned(),
            build_key: build_key.to_ascii_lowercase(),
        })
    }

    pub fn product(&self) -> &str {
        &self.product
    }
    pub fn build_key(&self) -> &str {
        &self.build_key
    }

    pub fn asset_root(&self, data_root: &Path) -> PathBuf {
        data_root
            .join("products")
            .join(&self.product)
            .join(&self.build_key)
    }

    /// `relative` is the model/texture path within this authored namespace.
    pub fn asset_path(&self, data_root: &Path, relative: impl AsRef<Path>) -> PathBuf {
        self.asset_root(data_root).join(relative)
    }
}
