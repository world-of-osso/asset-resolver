//! Runtime access policy. Extraction remains an explicit offline/development
//! capability; extracted-only consumers never initialize or read local CASC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AssetRuntimeMode {
    #[default]
    LocalCasc,
    ExtractedOnly,
}

impl std::str::FromStr for AssetRuntimeMode {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local-casc" => Ok(Self::LocalCasc),
            "extracted-only" => Ok(Self::ExtractedOnly),
            _ => Err(format!("unknown asset runtime mode: {value}")),
        }
    }
}
