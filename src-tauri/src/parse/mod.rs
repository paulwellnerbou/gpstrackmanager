pub mod fit;
pub mod gpx;

use std::path::Path;

use anyhow::Result;

use crate::model::ParsedTrack;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Gpx,
    Fit,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "gpx" => Some(Format::Gpx),
            "fit" => Some(Format::Fit),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Gpx => "gpx",
            Format::Fit => "fit",
        }
    }
}

/// Track formats found in archives that have no reader yet; they are catalogued as unsupported.
pub fn is_known_unsupported(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return false };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "nmea" | "kml" | "kmz" | "trl" | "itm" | "tcx" | "plt" | "igc" | "csv"
    )
}

pub fn parse(format: Format, bytes: &[u8]) -> Result<Vec<ParsedTrack>> {
    match format {
        Format::Gpx => gpx::parse(bytes),
        Format::Fit => fit::parse(bytes),
    }
}
