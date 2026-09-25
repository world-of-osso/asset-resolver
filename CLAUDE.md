# asset-resolver

AssetResolver trait + CASC implementation for extracting WoW assets from local CASC storage via cascette-rs.

## Structure

- `src/lib.rs` — `AssetResolver` trait definition + CASC implementation (behind `casc` feature flag)
- Feature `casc` (default) enables cascette-rs dependencies for local WoW CASC extraction

## Related

- cascette-rs: `~/Repos/cascette-rs` — Rust CASC/NGDP protocol implementation (used by casc-extract)
- casc-extract: `https://github.com/Osso/casc-extract` — CLI to regenerate `data/casc/` files from Blizzard CDN. Clone to /tmp, point deps at `~/Repos/cascette-rs`, run `cargo run -- init`.
- CASCLib: https://github.com/ladislav-zezula/CascLib — C library for reading CASC storage (WoW asset extraction reference)
