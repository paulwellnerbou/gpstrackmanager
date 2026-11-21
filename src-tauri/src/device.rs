//! GPS devices in USB mass-storage mode (Garmin eTrex and most handhelds and loggers).
//! Only well-known track folders are searched, so large data disks are never crawled.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Result, bail};
use chrono::Datelike;
use rusqlite::Connection;
use serde::Serialize;
use walkdir::WalkDir;

use crate::catalog::{self, Root};
use crate::parse::Format;
use crate::process::{FileImport, file_mtime, hash_bytes, process_file};

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

/// Content hashes of device files by (path, size, mtime), so a repeated check does not
/// read every file on the device again.
pub type HashCache = HashMap<(String, u64, i64), String>;

/// Volumes holding a library folder are skipped: they are archives, not devices.
/// `known` holds the hashes of all catalogued files.
pub fn find_devices(volumes_dir: &Path, roots: &[Root], known: &HashSet<String>, cache: &mut HashCache) -> Result<Vec<GpsDevice>> {
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
            let path = f.path().to_string_lossy().into_owned();
            let (size, mtime) = (meta.len(), file_mtime(&meta));
            let key = (path.clone(), size, mtime);
            let hash = match cache.get(&key) {
                Some(h) => Some(h.clone()),
                None => std::fs::read(f.path()).ok().map(|b| {
                    let h = hash_bytes(&b);
                    cache.insert(key, h.clone());
                    h
                }),
            };
            let imported = hash.is_some_and(|h| known.contains(&h));
            files.push(DeviceFile { path, name: f.file_name().to_string_lossy().into_owned(), size, mtime, imported });
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
    /// "path: reason" for every file that could not be imported.
    pub failed: Vec<String>,
}

/// Copies device files into `<root>/<year>/` and catalogs them. The device is never modified.
/// One failing file does not stop the others. The catalog is locked only while it is
/// queried or written, not while files are read and copied; `on_write` runs after each
/// catalog write while the lock is still held.
pub fn import_files(db: &Mutex<Connection>, root: &Root, files: &[String], on_write: impl Fn()) -> Result<DeviceImportResult> {
    let mut result = DeviceImportResult::default();
    for src in files {
        match import_one(db, root, Path::new(src), &on_write) {
            Ok(Some(dest)) => result.copied.push(dest),
            Ok(None) => result.skipped += 1,
            Err(e) => result.failed.push(format!("{src}: {e:#}")),
        }
    }
    Ok(result)
}

/// Garmin's running track log keeps growing between imports.
fn is_current_log(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("current.gpx"))
}

/// Returns the library path of the copy, or `None` if the file was imported before.
fn import_one(db: &Mutex<Connection>, root: &Root, src: &Path, on_write: &dyn Fn()) -> Result<Option<String>> {
    let Some(format) = Format::from_path(src) else { bail!("not a track file") };
    let parsed = process_file(src, format);
    if parsed.hash.is_empty() {
        // No hash means the file could not be read (e.g. the device was unplugged).
        bail!("{}", parsed.result.err().unwrap_or_else(|| "cannot read file".into()));
    }
    if catalog::hash_known(&db.lock().unwrap(), &parsed.hash)? {
        return Ok(None);
    }
    let first_start = parsed.result.as_ref().ok().and_then(|f| f.tracks.first()).and_then(|t| t.stats.start_time);
    let when = chrono::DateTime::from_timestamp_millis(first_start.unwrap_or(parsed.mtime));
    let dir = Path::new(&root.path).join(when.map(|d| d.year()).unwrap_or(1970).to_string());
    // A grown Current.gpx replaces the earlier copy of the same log (same first day)
    // instead of piling up Current-1.gpx, Current-2.gpx, … with duplicated tracks.
    let dest = if is_current_log(src) {
        let day = when.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "unknown".into());
        dir.join(format!("Current_{day}.gpx"))
    } else {
        unique_dest(&dir, src)?
    };
    std::fs::create_dir_all(&dir)?;
    // Copy under a name the scanner ignores, verify, then rename: a failed or partial copy
    // never shows up in the library.
    let tmp = dest.with_file_name(format!(".{}.part", dest.file_name().unwrap().to_string_lossy()));
    let verified = std::fs::copy(src, &tmp)
        .map_err(anyhow::Error::from)
        .and_then(|_| Ok(hash_bytes(&std::fs::read(&tmp)?) == parsed.hash));
    match verified {
        Ok(true) => std::fs::rename(&tmp, &dest)?,
        Ok(false) => {
            let _ = std::fs::remove_file(&tmp);
            bail!("the copy does not match the original");
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    let meta = std::fs::metadata(&dest)?;
    let imp = FileImport {
        path: dest.to_string_lossy().into_owned(),
        size: meta.len() as i64,
        mtime: file_mtime(&meta),
        hash: parsed.hash,
        format: parsed.format,
        result: parsed.result,
    };
    let mut conn = db.lock().unwrap();
    catalog::write_import(&mut conn, root.id, &imp)?;
    on_write();
    Ok(Some(imp.path))
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

    #[test]
    fn unreadable_file_fails_and_current_log_is_replaced() {
        let dir = std::env::temp_dir().join(format!("gtm-dev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (lib, dev) = (dir.join("lib"), dir.join("dev/Garmin/GPX/Current"));
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&dev).unwrap();
        let conn = catalog::open(&dir.join("c.sqlite")).unwrap();
        let root = catalog::add_root(&conn, lib.to_str().unwrap()).unwrap();
        let conn = Mutex::new(conn);

        let missing = dev.join("gone.gpx").to_string_lossy().into_owned();
        let r = import_files(&conn, &root, &[missing], || ()).unwrap();
        assert_eq!((r.copied.len(), r.skipped, r.failed.len()), (0, 0, 1));

        let pt = |m: u32| format!(r#"<trkpt lat="50.0{m}" lon="8.0"><time>2024-01-01T10:0{m}:00Z</time></trkpt>"#);
        let current = dev.join("Current.gpx");
        std::fs::write(&current, format!("<gpx><trk><trkseg>{}{}</trkseg></trk></gpx>", pt(1), pt(2))).unwrap();
        let first = import_files(&conn, &root, &[current.to_string_lossy().into_owned()], || ()).unwrap();
        std::fs::write(&current, format!("<gpx><trk><trkseg>{}{}{}</trkseg></trk></gpx>", pt(1), pt(2), pt(3))).unwrap();
        let second = import_files(&conn, &root, &[current.to_string_lossy().into_owned()], || ()).unwrap();
        assert_eq!(first.copied, second.copied, "the grown log replaces the earlier copy");
        assert_eq!(catalog::list_tracks(&conn.lock().unwrap()).unwrap().len(), 1);
        assert_eq!(std::fs::read_dir(lib.join("2024")).unwrap().count(), 1, "no partial or duplicate files");
        std::fs::remove_dir_all(&dir).ok();
    }
}
