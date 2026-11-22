pub mod fit;
pub mod gpx;
pub mod itm;
pub mod kml;
pub mod nmea;
pub mod trl;

use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result, anyhow};

use crate::model::ParsedFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Gpx,
    Fit,
    Kml,
    Kmz,
    Nmea,
    /// Holux M-241 binary track log.
    Trl,
    /// Holux ezTour project.
    Itm,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "gpx" => Some(Format::Gpx),
            "fit" => Some(Format::Fit),
            "kml" => Some(Format::Kml),
            "kmz" => Some(Format::Kmz),
            "nmea" => Some(Format::Nmea),
            "trl" => Some(Format::Trl),
            "itm" => Some(Format::Itm),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Gpx => "gpx",
            Format::Fit => "fit",
            Format::Kml => "kml",
            Format::Kmz => "kmz",
            Format::Nmea => "nmea",
            Format::Trl => "trl",
            Format::Itm => "itm",
        }
    }
}

/// Track formats found in archives that have no reader yet; they are catalogued as unsupported.
pub fn is_known_unsupported(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return false };
    matches!(ext.to_ascii_lowercase().as_str(), "tcx" | "plt" | "igc" | "csv")
}

pub fn parse(format: Format, bytes: &[u8]) -> Result<ParsedFile> {
    match format {
        Format::Gpx => gpx::parse(bytes),
        Format::Fit => fit::parse(bytes),
        Format::Kml => kml::parse(bytes),
        // Google Earth reads the first .kml in the archive, usually doc.kml.
        Format::Kmz => kml::parse(&zip_entry(bytes, |n| n.to_ascii_lowercase().ends_with(".kml"))?),
        Format::Nmea => nmea::parse(bytes),
        Format::Trl => trl::parse(bytes),
        Format::Itm => itm::parse(&zip_entry(bytes, |n| n.to_ascii_lowercase().ends_with(".poi"))?),
    }
}

/// The first entry of a ZIP archive whose name matches.
fn zip_entry(bytes: &[u8], pick: impl Fn(&str) -> bool) -> Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("not a ZIP archive")?;
    // ezTour writes names with a trailing NUL.
    let index = zip
        .file_names()
        .position(|n| n.is_ok_and(|n| pick(n.trim_end_matches('\0'))))
        .ok_or_else(|| anyhow!("no track data in the archive"))?;
    let mut entry = zip.by_index(index)?;
    let name = entry.name().map(|n| n.trim_end_matches('\0').to_string()).unwrap_or_default();
    // The size comes from the archive header; a corrupt one must not reserve gigabytes.
    let mut out = Vec::with_capacity(entry.size().min(64 << 20) as usize);
    entry.read_to_end(&mut out).with_context(|| format!("cannot unpack {name}"))?;
    Ok(out)
}
