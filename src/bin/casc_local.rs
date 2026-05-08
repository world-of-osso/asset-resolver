//! Extract files from local WoW CASC storage by FileDataID.
//!
//! Usage:
//!   cargo run --bin casc-local -- <fdid> [fdid2 ...] [-o output_dir]

use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (fdids, output_dir) = parse_args(&args);
    if fdids.is_empty() {
        eprintln!("Usage: casc-local <fdid> [fdid2 ...] [-o output_dir]");
        std::process::exit(1);
    }

    let (ok, fail) = extract_all(&fdids, &output_dir);
    eprintln!("{ok} extracted, {fail} failed");
    if fail > 0 {
        std::process::exit(1);
    }
}

fn parse_args(args: &[String]) -> (Vec<u32>, PathBuf) {
    let mut fdids = Vec::new();
    let mut output_dir = PathBuf::from(".");
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-o" && i + 1 < args.len() {
            output_dir = PathBuf::from(&args[i + 1]);
            i += 2;
        } else if let Ok(fdid) = args[i].parse::<u32>() {
            fdids.push(fdid);
            i += 1;
        } else {
            i += 1;
        }
    }
    (fdids, output_dir)
}

fn extract_all(fdids: &[u32], output_dir: &Path) -> (u32, u32) {
    let mut ok = 0u32;
    let mut fail = 0u32;
    for &fdid in fdids {
        match extract_fdid(fdid, output_dir) {
            Ok(path) => {
                eprintln!("Extracted FDID {fdid} -> {}", path.display());
                ok += 1;
            }
            Err(err) => {
                eprintln!("Failed FDID {fdid}: {err}");
                fail += 1;
            }
        }
    }
    (ok, fail)
}

fn extract_fdid(fdid: u32, output_dir: &Path) -> Result<PathBuf, String> {
    let out_path = output_dir.join(resolve_filename(fdid));
    if out_path.exists() {
        return Ok(out_path);
    }
    asset_resolver::casc_resolver::extract_fdid_to_path(fdid, &out_path)
}

fn resolve_filename(fdid: u32) -> String {
    let ext = asset_resolver::lookup_fdid(fdid)
        .and_then(extension_from_listfile_path)
        .unwrap_or("dat");
    format!("{fdid}.{ext}")
}

fn extension_from_listfile_path(path: &str) -> Option<&str> {
    path.rsplit_once('.').map(|(_, ext)| ext)
}

#[cfg(test)]
mod tests {
    use super::extension_from_listfile_path;

    #[test]
    fn resolves_extension_from_listfile_path_case_insensitively() {
        assert_eq!(
            extension_from_listfile_path("World/Maps/Test/Tile_1_2.ADT"),
            Some("ADT")
        );
    }

    #[test]
    fn resolves_extension_from_multi_dot_listfile_path() {
        assert_eq!(
            extension_from_listfile_path("spells/test.texture.BLP"),
            Some("BLP")
        );
    }
}
