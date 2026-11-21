//! GPS devices in USB mass-storage mode (Garmin eTrex and most handhelds and loggers).
//! Only well-known track folders are searched, so large data disks are never crawled.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use chrono::Datelike;
use rusqlite::Connection;
use serde::Serialize;
use walkdir::WalkDir;

use crate::catalog::{self, Root};
use crate::parse::Format;
use crate::process::{file_mtime, hash_bytes, process_file};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpsDevice {
    pub volume: String,
    pub model: Option<String>,
    pub files: Vec<DeviceFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFile {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub imported: bool,
}

const TRACK_DIRS: &[&str] = &["garmin", "gpx", "tracks", "track", "activity", "activities", "archive", "current"];

/// Volumes holding a library folder are skipped: they are archives, not devices.
pub fn find_devices(volumes_dir: &Path, conn: &Connection) -> Result<Vec<GpsDevice>> {
    let roots = catalog::list_roots(conn)?;
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(volumes_dir) else { return Ok(out) };
    for entry in entries.flatten() {
        let volume = entry.path();
        if roots.iter().any(|r| Path::new(&r.path).starts_with(&volume)) {
            continue;
        }
        let walker = WalkDir::new(&volume).max_depth(4).into_iter().filter_entry(|e| {
            e.depth() == 0
                || !e.file_type().is_dir()
                || TRACK_DIRS.contains(&e.file_name().to_string_lossy().to_ascii_lowercase().as_str())
        });
        let mut files = Vec::new();
        for f in walker.flatten() {
            let Ok(rel) = f.path().strip_prefix(&volume) else { continue };
            if !f.file_type().is_file() || !is_track_file(rel) {
                continue;
            }
            let Ok(meta) = f.metadata() else { continue };
            let imported = std::fs::read(f.path()).map(|b| catalog::hash_known(conn, &hash_bytes(&b))).unwrap_or(Ok(false))?;
            files.push(DeviceFile {
                path: f.path().to_string_lossy().into_owned(),
                name: f.file_name().to_string_lossy().into_owned(),
                size: meta.len(),
                mtime: file_mtime(&meta),
                imported,
            });
        }
        if files.is_empty() {
            continue;
        }
        let model = std::fs::read_to_string(volume.join("Garmin/GarminDevice.xml")).ok().and_then(|s| model_description(&s));
        files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        out.push(GpsDevice { volume: volume.to_string_lossy().into_owned(), model, files });
    }
    Ok(out)
}

/// `rel` is relative to the volume root. Activity FITs live in `Activity`/`Activities`
/// (other FITs on Garmins are settings); GPX files sit in the root or a track folder.
fn is_track_file(rel: &Path) -> bool {
    let dirs: Vec<String> = rel
        .parent()
        .into_iter()
        .flat_map(|p| p.components())
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    if rel.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
        return false;
    }
    match Format::from_path(rel) {
        Some(Format::Fit) => dirs.iter().any(|d| d == "activity" || d == "activities"),
        Some(Format::Gpx) => dirs.is_empty() || dirs.iter().any(|d| d != "garmin"),
        None => false,
    }
}

fn model_description(xml: &str) -> Option<String> {
    let model = &xml[xml.find("<Model>")?..];
    let start = model.find("<Description>")? + "<Description>".len();
    let end = model[start..].find("</Description>")? + start;
    Some(model[start..end].trim().to_string())
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeviceImportResult {
    pub copied: Vec<String>,
    pub skipped: usize,
}

/// Copies device files into `<root>/<year>/` and catalogs them. The device is never modified.
pub fn import_files(conn: &mut Connection, root: &Root, files: &[String]) -> Result<DeviceImportResult> {
    let mut result = DeviceImportResult::default();
    for src in files {
        let src = Path::new(src);
        let Some(format) = Format::from_path(src) else { bail!("not a track file: {}", src.display()) };
        let parsed = process_file(src, format);
        if catalog::hash_known(conn, &parsed.hash)? {
            result.skipped += 1;
            continue;
        }
        let when = parsed
            .result
            .as_ref()
            .ok()
            .and_then(|f| f.tracks.first())
            .and_then(|t| t.stats.start_time)
            .unwrap_or(parsed.mtime);
        let year = chrono::DateTime::from_timestamp_millis(when).map(|d| d.year()).unwrap_or(1970);
        let dest = unique_dest(&Path::new(&root.path).join(year.to_string()), src)?;
        std::fs::create_dir_all(dest.parent().unwrap())?;
        std::fs::copy(src, &dest)?;
        let mut imp = process_file(&dest, format);
        if imp.hash != parsed.hash {
            bail!("copy of {} does not match the original", src.display());
        }
        imp.result = parsed.result;
        catalog::write_import(conn, root.id, &imp)?;
        result.copied.push(dest.to_string_lossy().into_owned());
    }
    Ok(result)
}

fn unique_dest(dir: &Path, src: &Path) -> Result<PathBuf> {
    let name = src.file_name().unwrap();
    let mut dest = dir.join(name);
    let stem = src.file_stem().unwrap().to_string_lossy();
    let ext = src.extension().map(|e| e.to_string_lossy()).unwrap_or_default();
    let mut n = 1;
    while dest.exists() {
        dest = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_model_and_filters_files() {
        let xml = "<Device><Model><PartNumber>006-B</PartNumber><Description>eTrex 32x</Description></Model></Device>";
        assert_eq!(model_description(xml).as_deref(), Some("eTrex 32x"));
        assert!(is_track_file(Path::new("Garmin/Activity/2024-05-15.fit")));
        assert!(!is_track_file(Path::new("Garmin/Settings/Settings.fit")));
        assert!(is_track_file(Path::new("Garmin/GPX/Current/Current.gpx")));
        assert!(is_track_file(Path::new("track_001.gpx")));
        assert!(!is_track_file(Path::new("._track_001.gpx")));
    }
}
