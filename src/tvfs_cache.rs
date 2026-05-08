use std::collections::HashMap;

use cascette_crypto::{ContentKey, EncodingKey};
use cascette_formats::tvfs::TvfsManifest;

pub type TvfsResolutionMap = HashMap<u32, (ContentKey, EncodingKey)>;

pub fn collect_wow_tvfs_resolutions(data: &[u8]) -> Result<TvfsResolutionMap, String> {
    let manifest = TvfsManifest::parse(data).map_err(|e| e.to_string())?;
    let mut resolutions = HashMap::new();
    for entry in manifest.entries {
        let (Some(fdid), Some(content_key)) = (entry.file_data_id, entry.content_key) else {
            continue;
        };
        resolutions.insert(fdid, (content_key, entry.encoding_key));
    }
    Ok(resolutions)
}
