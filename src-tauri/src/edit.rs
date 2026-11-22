//! Point editing. Edits change the catalog copy; `revert_file` discards them, and only
//! `write_to_file`, on the user's request, writes them into the file.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use rusqlite::{Connection, Transaction, params};

use crate::catalog::{self, TrackData};
use crate::cleanup::{self, StayOptions, is_zero_fix};
use crate::geo::{bearing_deg, haversine_m};
use crate::model::{NO_TIME, ParsedTrack, Points};
use crate::parse::Format;
use crate::process::{ProcessedTrack, process_file, process_track};
use crate::undo;

pub const POINT_COLUMNS: usize = 10;

/// Flat rows of `[lat, lon, ele, time_ms, segment_start, distance_m, speed_ms, bearing_deg,
/// recorded_speed_ms, recorded_course_deg]`. NaN marks unknown values. `segment_start` is 1
/// where one of the file's segments begins, 2 where the line is only broken at a glitch or
/// recording gap, else 0. The recorded columns are what the file holds; the others are calculated.
/// Distance does not include gaps between segments. Speed and bearing are taken over
/// both neighbours within the segment, which smooths single-fix jitter.
pub fn point_table(data: &TrackData) -> Vec<f64> {
    let n = data.coords.len();
    let mut is_start = vec![false; n];
    let mut seg_end = vec![n; n];
    let mut seg_begin = vec![0; n];
    for (k, &s) in data.seg_starts.iter().enumerate() {
        let e = data.seg_starts.get(k + 1).map(|&e| e as usize).unwrap_or(n);
        if (s as usize) < e {
            is_start[s as usize] = true;
            for i in s as usize..e {
                seg_begin[i] = s as usize;
                seg_end[i] = e;
            }
        }
    }
    let step = |a: usize, b: usize| {
        let (p, q) = (data.coords[a], data.coords[b]);
        haversine_m(p.0, p.1, q.0, q.1)
    };
    let mut dist = vec![0f64; n];
    for i in 1..n {
        dist[i] = dist[i - 1] + if is_start[i] { 0.0 } else { step(i - 1, i) };
    }
    let time = |i: usize| if data.time[i] == NO_TIME { f64::NAN } else { data.time[i] as f64 };

    let mut out = Vec::with_capacity(n * POINT_COLUMNS);
    for i in 0..n {
        let a = if i > seg_begin[i] { i - 1 } else { i };
        let b = if i + 1 < seg_end[i] { i + 1 } else { i };
        let dt = (time(b) - time(a)) / 1000.0;
        let d = dist[b] - dist[a];
        let speed = if dt > 0.0 { d / dt } else { f64::NAN };
        let bearing = if a != b && d > 0.0 {
            let (p, q) = (data.coords[a], data.coords[b]);
            bearing_deg(p.0, p.1, q.0, q.1)
        } else {
            f64::NAN
        };
        let (lat, lon) = data.coords[i];
        out.extend_from_slice(&[
            lat,
            lon,
            data.ele[i] as f64,
            time(i),
            if is_start[i] { 2.0 } else { 0.0 },
            dist[i],
            speed,
            bearing,
            data.speed[i] as f64,
            data.course[i] as f64,
        ]);
    }
    // Catalogs written before file segments were normalised may list a start at `n`.
    for &s in data.file_seg_starts.iter().filter(|&&s| (s as usize) < n) {
        out[s as usize * POINT_COLUMNS + 4] = 1.0;
    }
    out
}

/// Builds points from the kept indices (ascending). Removing points inside a segment
/// joins their neighbours; segment boundaries of the original are preserved.
fn subset(data: &TrackData, keep: &[usize]) -> Points {
    let mut p = Points::default();
    let mut si = 0;
    let mut prev: Option<usize> = None;
    for &i in keep {
        let mut new_segment = prev.is_none();
        let starts = &data.file_seg_starts;
        while si < starts.len() && starts[si] as usize <= i {
            if prev.is_none_or(|pv| starts[si] as usize > pv) {
                new_segment = true;
            }
            si += 1;
        }
        if new_segment {
            p.start_segment();
        }
        let (lat, lon) = data.coords[i];
        let ele = Some(data.ele[i]).filter(|e| !e.is_nan());
        let time = Some(data.time[i]).filter(|&t| t != NO_TIME);
        let recorded = |v: &[f32]| v.get(i).copied().filter(|x| !x.is_nan());
        p.push_recorded(lat, lon, ele, time, recorded(&data.speed), recorded(&data.course));
        prev = Some(i);
    }
    p
}

fn process(points: Points) -> ProcessedTrack {
    process_track(ParsedTrack { name: None, description: None, activity: None, points })
}

fn write_geometry(tx: &Transaction, track_id: i64, t: &ProcessedTrack) -> Result<()> {
    let s = &t.stats;
    let bbox = s.bbox.map(|b| b.map(Some)).unwrap_or([None; 4]);
    tx.execute(
        "UPDATE tracks SET start_time = ?2, end_time = ?3, point_count = ?4, distance_m = ?5,
           min_lat = ?6, min_lon = ?7, max_lat = ?8, max_lon = ?9, min_ele = ?10, max_ele = ?11, edited = 1
         WHERE id = ?1",
        params![
            track_id, s.start_time, s.end_time, s.point_count as i64, s.distance_m,
            bbox[0], bbox[1], bbox[2], bbox[3], s.min_ele, s.max_ele
        ],
    )?;
    catalog::insert_track_data(tx, track_id, &t.blobs)?;
    catalog::write_places(tx, track_id, &t.places)?;
    Ok(())
}

pub fn delete_points(conn: &mut Connection, track_id: i64, indices: &[u32]) -> Result<()> {
    let data = catalog::load_track_data(conn, track_id)?;
    let n = data.coords.len();
    let mut delete = vec![false; n];
    for &i in indices {
        if let Some(d) = delete.get_mut(i as usize) {
            *d = true;
        }
    }
    let keep: Vec<usize> = (0..n).filter(|&i| !delete[i]).collect();
    if keep.len() < 2 {
        bail!("A track needs at least two points.");
    }
    let processed = process(subset(&data, &keep));
    let removed = n - keep.len();
    let tx = catalog::write_tx(conn)?;
    let label = if removed == 1 { "Delete point".to_string() } else { format!("Delete {removed} points") };
    undo::record(&tx, &label, &[track_id], None)?;
    write_geometry(&tx, track_id, &processed)?;
    tx.commit()?;
    Ok(())
}

/// Collapses every stay into its spot (when `stays` is given): the stay's first and last
/// points move there and keep the arrival and departure times, the fixes in between are
/// removed. With `drop_zero` and `drop_spikes`, also removes the zero fixes and spikes.
/// One undo step. Returns the number of points removed.
pub fn clean_up(
    conn: &mut Connection,
    track_id: i64,
    stays: Option<StayOptions>,
    drop_zero: bool,
    drop_spikes: bool,
) -> Result<usize> {
    let mut data = catalog::load_track_data(conn, track_id)?;
    let n = data.coords.len();
    let plan = cleanup::plan(&data.coords, &data.ele, &data.time, &data.file_seg_starts, stays, drop_zero, drop_spikes);
    let mut drop: Vec<bool> = data.coords.iter().map(|&c| drop_zero && is_zero_fix(c)).collect();
    for &i in &plan.spikes {
        drop[i as usize] = true;
    }
    for s in &plan.stays {
        let (a, b) = (s.start as usize, s.end as usize);
        for i in [a, b] {
            data.coords[i] = (s.lat, s.lon);
            data.ele[i] = s.ele;
        }
        drop[a + 1..b].fill(true);
    }
    let keep: Vec<usize> = (0..n).filter(|&i| !drop[i]).collect();
    let removed = n - keep.len();
    if removed == 0 {
        return Ok(0);
    }
    if keep.len() < 2 {
        bail!("A track needs at least two points.");
    }
    let processed = process(subset(&data, &keep));
    let tx = catalog::write_tx(conn)?;
    undo::record(&tx, &format!("Clean up ({removed} points)"), &[track_id], None)?;
    write_geometry(&tx, track_id, &processed)?;
    tx.commit()?;
    Ok(removed)
}

/// Splits before point `at`; the original keeps `[0, at)`, a new track gets the rest.
/// Returns the new track's id.
pub fn split(conn: &mut Connection, track_id: i64, at: u32) -> Result<i64> {
    let n = catalog::load_track_data(conn, track_id)?.coords.len() as u32;
    if at < 2 || at + 2 > n {
        bail!("Both tracks need at least two points.");
    }
    extract_as(conn, track_id, at, n, "Split track")
}

/// Moves points `[start, end)` into a new track (e.g. one segment); the original keeps
/// the rest. Returns the new track's id.
pub fn extract(conn: &mut Connection, track_id: i64, start: u32, end: u32) -> Result<i64> {
    extract_as(conn, track_id, start, end, "Make separate track")
}

fn extract_as(conn: &mut Connection, track_id: i64, start: u32, end: u32, label: &str) -> Result<i64> {
    let data = catalog::load_track_data(conn, track_id)?;
    let (n, start, end) = (data.coords.len(), start as usize, (end as usize).min(data.coords.len()));
    let rest: Vec<usize> = (0..start).chain(end..n).collect();
    if rest.len() < 2 || end < start + 2 {
        bail!("Both tracks need at least two points.");
    }
    let first = process(subset(&data, &rest));
    let second = process(subset(&data, &(start..end).collect::<Vec<_>>()));

    let tx = catalog::write_tx(conn)?;
    let (file_id, path, base): (i64, String, Option<String>) = tx.query_row(
        "SELECT t.file_id, f.path, COALESCE(t.title, t.name) FROM tracks t JOIN files f ON f.id = t.file_id
         WHERE t.id = ?1",
        [track_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    // An untitled part is named by its own route; the file name is the last resort.
    let title = match base {
        Some(base) => Some(format!("{base} (part 2)")),
        None if !second.places.is_empty() => None,
        None => {
            let file = Path::new(&path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(path.clone());
            Some(format!("{file} (part 2)"))
        }
    };
    tx.execute(
        "INSERT INTO tracks(file_id, idx, name, file_description, title, description, file_activity, activity,
           point_count, distance_m, edited)
         SELECT file_id, (SELECT MIN(0, MIN(idx)) FROM tracks WHERE file_id = ?2) - 1,
                name, file_description, ?3, description, file_activity, activity, 0, 0, 1
         FROM tracks WHERE id = ?1",
        params![track_id, file_id, title],
    )?;
    let new_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO track_tags(track_id, tag_id) SELECT ?2, tag_id FROM track_tags WHERE track_id = ?1",
        params![track_id, new_id],
    )?;
    undo::record(&tx, label, &[track_id], Some(new_id))?;
    write_geometry(&tx, track_id, &first)?;
    write_geometry(&tx, new_id, &second)?;
    tx.commit()?;
    Ok(new_id)
}

/// Merges the file segment starting at point `at` into the previous one.
pub fn join_segment(conn: &mut Connection, track_id: i64, at: u32) -> Result<()> {
    let mut data = catalog::load_track_data(conn, track_id)?;
    if at == 0 || !data.file_seg_starts.contains(&at) {
        bail!("No segment starts at point {}.", at + 1);
    }
    data.file_seg_starts.retain(|&s| s != at);
    let processed = process(subset(&data, &(0..data.coords.len()).collect::<Vec<_>>()));
    let tx = catalog::write_tx(conn)?;
    undo::record(&tx, "Join segments", &[track_id], None)?;
    write_geometry(&tx, track_id, &processed)?;
    tx.commit()?;
    Ok(())
}

/// Drops all edits of a file's tracks (including split-off parts) and re-imports it.
/// Titles, descriptions and tags of the file's own tracks are kept.
pub fn revert_file(conn: &mut Connection, file_id: i64) -> Result<()> {
    let (root_id, path): (i64, String) =
        conn.query_row("SELECT root_id, path FROM files WHERE id = ?1", [file_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let Some(format) = Format::from_path(Path::new(&path)) else { bail!("unsupported file: {path}") };
    let imported = process_file(Path::new(&path), format);
    if let Err(e) = &imported.result {
        bail!("cannot read {path}: {e}");
    }
    let tx = catalog::write_tx(conn)?;
    tx.execute("DELETE FROM tracks WHERE file_id = ?1 AND idx < 0", [file_id])?;
    tx.execute("UPDATE tracks SET edited = 0 WHERE file_id = ?1", [file_id])?;
    catalog::write_import_in(&tx, root_id, &imported)?;
    tx.commit()?;
    Ok(())
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WrittenFile {
    pub path: String,
    /// The file as it was before, renamed next to it.
    pub backup: String,
}

/// Writes the catalog version of a file (all its tracks, split-off parts as tracks of their
/// own, and its waypoints) as GPX over the original, which is kept as a `.bak` the scanner
/// ignores. A file in another format (FIT, KML, …) becomes a GPX file of the same name.
/// Afterwards the file's tracks are plain tracks of the new file: not edited, no undo
/// history, user metadata kept.
///
/// Only what the catalog holds is written: points the import skipped and data it does not
/// read (sensor data, other extensions) are only in the backup.
pub fn write_to_file(conn: &mut Connection, file_id: i64) -> Result<WrittenFile> {
    let (root_id, path): (i64, String) =
        conn.query_row("SELECT root_id, path FROM files WHERE id = ?1", [file_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let source = PathBuf::from(&path);
    let Some(format) = Format::from_path(&source) else { bail!("unsupported file: {path}") };
    if !source.is_file() {
        bail!("{path} is missing.");
    }
    let target = if format == Format::Gpx { source.clone() } else { source.with_extension("gpx") };
    if target != source && target.exists() {
        bail!("{} already exists.", target.display());
    }

    // File order, then split-off parts oldest first, as the track list shows them.
    // (id, edited, [name, description, activity]) as the user sees them.
    let rows: Vec<(i64, bool, [Option<String>; 3])> = {
        let mut st = conn.prepare(
            "SELECT id, edited, COALESCE(title, name), COALESCE(description, file_description),
                    COALESCE(activity, file_activity)
             FROM tracks WHERE file_id = ?1 ORDER BY idx < 0, abs(idx)",
        )?;
        st.query_map([file_id], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0, [r.get(2)?, r.get(3)?, r.get(4)?])))?
            .collect::<rusqlite::Result<_>>()?
    };
    if !rows.iter().any(|r| r.1) {
        bail!("Nothing in this file is edited.");
    }
    let mut tracks = Vec::with_capacity(rows.len());
    for (id, _, [name, description, activity]) in &rows {
        let data = catalog::load_track_data(conn, *id)?;
        tracks.push(crate::export::GpxTrack {
            name: name.clone(),
            description: description.clone(),
            activity: activity.clone(),
            data,
        });
    }
    let waypoints = catalog::list_file_waypoints(conn, file_id)?;

    let dir = target.parent().unwrap_or(Path::new("."));
    // Hidden, so a scan running meanwhile skips it.
    let tmp = dir.join(format!(".{}.part", target.file_name().unwrap_or_default().to_string_lossy()));
    let mut part = TempFile { path: &tmp, keep: false };
    let mut imported = write_verified(&tmp, &source, &tracks, &waypoints)
        .map_err(|e| e.context(format!("writing {} failed; the file is unchanged", target.display())))?;
    imported.path = target.to_string_lossy().into_owned();

    let tx = catalog::write_tx(conn)?;
    // The device may still hold the replaced contents; the import must keep seeing them as imported.
    tx.execute(
        "INSERT OR IGNORE INTO written_hashes(hash, file_id) SELECT hash, id FROM files WHERE id = ?1 AND hash != ''",
        [file_id],
    )?;
    if target != source {
        tx.execute("UPDATE files SET path = ?2, format = 'gpx' WHERE id = ?1", params![file_id, imported.path])?;
    }
    // Every track takes its position in the new file, so the import matches it to its own
    // row (keeping title, tags, activity) and replaces the geometry with what was written.
    tx.execute("UPDATE tracks SET idx = idx - (1 << 42) WHERE file_id = ?1", [file_id])?;
    for (k, (id, _, [name, ..])) in rows.iter().enumerate() {
        tx.execute("UPDATE tracks SET idx = ?2, name = ?3, edited = 0 WHERE id = ?1", params![id, k as i64, name])?;
    }
    catalog::write_import_in(&tx, root_id, &imported)?;

    let backup = free_backup_path(&source)?;
    let restore = || {
        if target != source {
            let _ = std::fs::remove_file(&target);
        }
        let _ = std::fs::rename(&backup, &source);
    };
    if let Err(e) = std::fs::rename(&source, &backup) {
        bail!("cannot keep {path} as {}: {e}", backup.display());
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        restore();
        bail!("cannot write {}: {e}", target.display());
    }
    part.keep = true;
    if let Err(e) = tx.commit() {
        restore();
        return Err(e.into());
    }
    Ok(WrittenFile { path: imported.path, backup: backup.to_string_lossy().into_owned() })
}

/// Deleted when dropped unless kept: a write that fails at any step leaves no copy behind.
struct TempFile<'a> {
    path: &'a Path,
    keep: bool,
}

impl Drop for TempFile<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(self.path);
        }
    }
}

/// Writes the GPX to `tmp`, carrying over what the file system keeps about `source`, and
/// reads it back: the original is only replaced by a file that yields exactly the
/// catalog's points and waypoints.
fn write_verified(
    tmp: &Path,
    source: &Path,
    tracks: &[crate::export::GpxTrack],
    waypoints: &[catalog::WaypointRow],
) -> Result<crate::process::FileImport> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(tmp)?);
    crate::export::gpx(&mut out, tracks, waypoints)?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    let meta = std::fs::metadata(source)?;
    file.set_permissions(meta.permissions())?;
    copy_file_attributes(source, &meta, tmp, &file);
    file.sync_all()?;
    let imported = process_file(tmp, Format::Gpx);
    let read_back = imported.result.as_ref().map_err(|e| anyhow::anyhow!("cannot read the written file: {e}"))?;
    if let Some(problem) = read_back_mismatch(read_back, tracks, waypoints) {
        bail!("the written file does not read back as the catalog holds it: {problem}");
    }
    Ok(imported)
}

fn read_back_mismatch(
    file: &crate::process::ProcessedFile,
    tracks: &[crate::export::GpxTrack],
    waypoints: &[catalog::WaypointRow],
) -> Option<String> {
    use crate::codec;
    if file.tracks.len() != tracks.len() {
        return Some(format!("{} tracks instead of {}", file.tracks.len(), tracks.len()));
    }
    let same_f32 = |a: &f32, b: &f32| a == b || (a.is_nan() && b.is_nan());
    for (k, (got, want)) in file.tracks.iter().zip(tracks).enumerate() {
        let (b, d) = (&got.blobs, &want.data);
        let n = d.coords.len();
        // The writer leaves out empty segments.
        let segs: Vec<u32> = (d.file_seg_starts.iter().enumerate())
            .filter(|&(j, &s)| (s as usize) < d.file_seg_starts.get(j + 1).map_or(n, |&e| e as usize).min(n))
            .map(|(_, &s)| s)
            .collect();
        let ele = codec::decode_f32(&b.ele);
        let same = |got: &[f32], want: &[f32]| got.len() == want.len() && got.iter().zip(want).all(|(a, b)| same_f32(a, b));
        let recorded = |blob: &Option<Vec<u8>>| blob.as_deref().map_or_else(|| vec![f32::NAN; n], codec::decode_f32);
        let problem = if codec::decode_coords(&b.coords) != d.coords {
            "positions"
        } else if !same(&ele, &d.ele) {
            "elevations"
        } else if codec::decode_i64(&b.time) != d.time {
            "times"
        } else if codec::decode_u32(&b.file_segs) != segs {
            "segments"
        } else if !same(&recorded(&b.speed), &d.speed) {
            "speeds"
        } else if !same(&recorded(&b.course), &d.course) {
            "courses"
        } else {
            continue;
        };
        return Some(format!("{problem} of track {} differ", k + 1));
    }
    let same_waypoint = |r: &crate::model::Waypoint, w: &catalog::WaypointRow| {
        (r.lat - w.lat).abs() < 1e-7
            && (r.lon - w.lon).abs() < 1e-7
            && r.ele.map(f64::from) == w.ele
            && r.time == w.time
    };
    if file.waypoints.len() != waypoints.len() || !file.waypoints.iter().zip(waypoints).all(|(r, w)| same_waypoint(r, w)) {
        return Some("waypoints differ".into());
    }
    None
}

/// Finder tags and comments, other extended attributes and the creation date stay with the
/// file. Best effort: some attributes are the system's to set, and the backup keeps them all.
fn copy_file_attributes(source: &Path, meta: &std::fs::Metadata, tmp: &Path, file: &std::fs::File) {
    #[cfg(unix)]
    if let Ok(names) = xattr::list(source) {
        for name in names {
            if let Ok(Some(value)) = xattr::get(source, &name) {
                let _ = xattr::set(tmp, &name, &value);
            }
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(created) = meta.created() {
        use std::os::macos::fs::FileTimesExt;
        let _ = file.set_times(std::fs::FileTimes::new().set_created(created));
    }
    let _ = (source, meta, tmp, file);
}

/// `name.bak`, or `name.2.bak`, … when earlier writes left backups: none is overwritten.
fn free_backup_path(source: &Path) -> Result<PathBuf> {
    let name = source.file_name().unwrap_or_default().to_string_lossy();
    (1..1000)
        .map(|n| source.with_file_name(if n == 1 { format!("{name}.bak") } else { format!("{name}.{n}.bak") }))
        .find(|p| !p.exists())
        .ok_or_else(|| anyhow::anyhow!("too many backups of {}", source.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize, seg_starts: Vec<u32>) -> TrackData {
        TrackData {
            coords: (0..n).map(|i| (1.0 + i as f64 * 0.001, 2.0)).collect(),
            ele: vec![f32::NAN; n],
            time: vec![NO_TIME; n],
            importance: vec![0.0; n],
            file_seg_starts: seg_starts.clone(),
            seg_starts,
            speed: vec![f32::NAN; n],
            course: vec![f32::NAN; n],
        }
    }

    #[test]
    fn deleting_inside_a_segment_joins_and_keeps_boundaries() {
        let d = data(6, vec![0, 3]);
        let p = subset(&d, &[0, 2, 3, 5]);
        assert_eq!(p.len(), 4);
        assert_eq!(p.seg_starts, vec![0, 2]);
        // Deleting the first point of a segment still starts a new segment at the next kept one.
        let p = subset(&d, &[0, 1, 4, 5]);
        assert_eq!(p.seg_starts, vec![0, 2]);
    }

    #[test]
    fn split_delete_and_revert_roundtrip() {
        let dir = std::env::temp_dir().join(format!("gtm-edit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("t.gpx");
        let pts: String = (0..10)
            .map(|i| format!(r#"<trkpt lat="50.{i:03}" lon="8.0"><time>2024-01-01T10:00:{i:02}Z</time></trkpt>"#))
            .collect();
        std::fs::write(&gpx, format!("<gpx><trk><name>T</name><trkseg>{pts}</trkseg></trk></gpx>")).unwrap();

        let mut conn = catalog::open(&dir.join("c.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let id = catalog::list_tracks(&conn).unwrap()[0].id;
        catalog::set_track_tags(&mut conn, id, &["walk".into()]).unwrap();

        let new_id = split(&mut conn, id, 4).unwrap();
        delete_points(&mut conn, new_id, &[0, 1]).unwrap();
        let tracks = catalog::list_tracks(&conn).unwrap();
        let count = |id| tracks.iter().find(|t| t.id == id).unwrap().point_count;
        assert_eq!((count(id), count(new_id)), (4, 4));
        let part2 = tracks.iter().find(|t| t.id == new_id).unwrap();
        assert_eq!(part2.title.as_deref(), Some("T (part 2)"));
        assert_eq!(part2.tags, vec!["walk".to_string()]);
        assert!(part2.edited);

        // A rescan of an unchanged-in-catalog but re-imported file must not undo edits.
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        assert_eq!(catalog::list_tracks(&conn).unwrap().iter().find(|t| t.id == id).unwrap().point_count, 4);

        revert_file(&mut conn, part2.file_id).unwrap();
        let tracks = catalog::list_tracks(&conn).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!((tracks[0].id, tracks[0].point_count, tracks[0].edited), (id, 10, false));
        assert_eq!(tracks[0].tags, vec!["walk".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writing_edits_to_the_file_keeps_a_backup_and_the_catalog_metadata() {
        let dir = std::env::temp_dir().join(format!("gtm-write-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("t.gpx");
        let a: String = (0..10)
            .map(|i| {
                format!(
                    r#"<trkpt lat="50.{i:03}1234" lon="8.0"><ele>1{i}2.5</ele><time>2024-01-01T10:00:0{i}.250Z</time><speed>1</speed></trkpt>"#
                )
            })
            .collect();
        let b: String = (0..5)
            .map(|i| format!(r#"<trkpt lat="51.{i:03}" lon="9.0"><time>2024-01-02T10:00:0{i}Z</time></trkpt>"#))
            .collect();
        let original = format!(
            r#"<gpx><wpt lat="50.5" lon="8.5"><name>Hut &amp; view</name><sym>Flag</sym></wpt>
            <trk><name>A</name><type>hiking</type><trkseg>{a}<trkpt lat="0" lon="0"/></trkseg></trk>
            <trk><name>B</name><trkseg>{b}</trkseg></trk></gpx>"#
        );
        std::fs::write(&gpx, &original).unwrap();
        #[cfg(unix)]
        xattr::set(&gpx, "com.example.gtm-test", b"tag").unwrap();
        let created = std::fs::metadata(&gpx).unwrap().created().ok();

        let mut conn = catalog::open(&dir.join("c.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let tracks = catalog::list_tracks(&conn).unwrap();
        let id_of = |name: &str| tracks.iter().find(|t| t.name.as_deref() == Some(name)).unwrap().id;
        let (a_id, b_id, file_id) = (id_of("A"), id_of("B"), tracks[0].file_id);
        assert!(write_to_file(&mut conn, file_id).is_err(), "nothing edited yet");

        catalog::set_track_tags(&mut conn, a_id, &["walk".into()]).unwrap();
        catalog::update_track_meta(&conn, b_id, Some("Bee"), None).unwrap();
        let part = split(&mut conn, a_id, 4).unwrap();
        delete_points(&mut conn, part, &[0]).unwrap();
        let before = catalog::load_track_data(&conn, a_id).unwrap();

        let w = write_to_file(&mut conn, file_id).unwrap();
        assert_eq!(w.path, gpx.to_string_lossy());
        assert_eq!(std::fs::read_to_string(dir.join("t.gpx.bak")).unwrap(), original);
        #[cfg(unix)]
        assert_eq!(xattr::get(&gpx, "com.example.gtm-test").unwrap().as_deref(), Some(&b"tag"[..]));
        #[cfg(target_os = "macos")]
        assert_eq!(std::fs::metadata(&gpx).unwrap().created().ok(), created);
        let _ = created;

        let tracks = catalog::list_tracks(&conn).unwrap();
        let get = |id| tracks.iter().find(|t| t.id == id).unwrap();
        assert_eq!(tracks.len(), 3);
        assert!(tracks.iter().all(|t| !t.edited && t.idx >= 0));
        assert_eq!((get(a_id).idx, get(b_id).idx, get(part).idx), (0, 1, 2));
        assert_eq!((get(a_id).point_count, get(b_id).point_count, get(part).point_count), (4, 5, 5));
        assert_eq!(get(part).title.as_deref(), Some("A (part 2)"));
        assert_eq!(get(part).tags, vec!["walk".to_string()]);
        assert_eq!(get(b_id).title.as_deref(), Some("Bee"));
        assert_eq!(get(part).file_activity.as_deref(), Some("hiking"));
        assert!(undo::last_edit(&conn, file_id).unwrap().is_none());
        let after = catalog::load_track_data(&conn, a_id).unwrap();
        assert_eq!((after.coords, after.ele, after.time), (before.coords, before.ele, before.time));
        assert_eq!((before.speed.clone(), after.speed), (vec![1.0; 4], before.speed), "recorded speed is written");
        assert!(std::fs::read_to_string(&gpx).unwrap().contains("<gpxtpx:speed>1</gpxtpx:speed>"));
        let wpts = catalog::list_file_waypoints(&conn, file_id).unwrap();
        assert_eq!((wpts.len(), wpts[0].name.as_deref(), wpts[0].symbol.as_deref()), (1, Some("Hut & view"), Some("Flag")));

        // The written file is now what "Revert to file" goes back to.
        revert_file(&mut conn, file_id).unwrap();
        assert_eq!(catalog::list_tracks(&conn).unwrap().len(), 3);

        // A second write never overwrites the first backup.
        delete_points(&mut conn, b_id, &[0]).unwrap();
        let w = write_to_file(&mut conn, file_id).unwrap();
        assert_eq!(w.backup, dir.join("t.gpx.2.bak").to_string_lossy());
        assert_eq!(std::fs::read_to_string(dir.join("t.gpx.bak")).unwrap(), original);
        assert!(!dir.join(".t.gpx.part").exists());

        // A write that fails after the temporary file was written leaves the file as it was
        // and no temporary file behind.
        for n in 3..1000 {
            std::fs::write(dir.join(format!("t.gpx.{n}.bak")), "").unwrap();
        }
        let current = std::fs::read(&gpx).unwrap();
        delete_points(&mut conn, b_id, &[0]).unwrap();
        let e = write_to_file(&mut conn, file_id).unwrap_err();
        assert!(format!("{e:#}").contains("too many backups"), "{e:#}");
        assert_eq!(std::fs::read(&gpx).unwrap(), current);
        assert!(!dir.join(".t.gpx.part").exists());
        assert!(catalog::list_tracks(&conn).unwrap().iter().any(|t| t.edited), "the catalog keeps the edit");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_back_compares_values_not_only_counts() {
        let dir = std::env::temp_dir().join(format!("gtm-readback-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = data(6, vec![0, 3]);
        d.ele = vec![100.5, f32::NAN, 102.0, 103.25, 104.0, 105.0];
        d.time = (0..6).map(|i| 1_700_000_000_250 + i * 1000).collect();
        let mut tracks = vec![crate::export::GpxTrack { name: Some("T".into()), description: None, activity: None, data: d }];
        let tmp = dir.join("t.gpx");
        let mut out = Vec::new();
        crate::export::gpx(&mut out, &tracks, &[]).unwrap();
        std::fs::write(&tmp, out).unwrap();
        let imported = process_file(&tmp, Format::Gpx);
        let file = imported.result.as_ref().unwrap();
        assert_eq!(read_back_mismatch(file, &tracks, &[]), None);

        tracks[0].data.ele[1] = 101.0;
        assert_eq!(read_back_mismatch(file, &tracks, &[]).as_deref(), Some("elevations of track 1 differ"));
        tracks[0].data.ele[1] = f32::NAN;
        tracks[0].data.time[2] += 1;
        assert_eq!(read_back_mismatch(file, &tracks, &[]).as_deref(), Some("times of track 1 differ"));
        tracks[0].data.time[2] -= 1;
        tracks[0].data.file_seg_starts = vec![0];
        assert_eq!(read_back_mismatch(file, &tracks, &[]).as_deref(), Some("segments of track 1 differ"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clean_up_collapses_a_stay_and_keeps_its_times() {
        let dir = std::env::temp_dir().join(format!("gtm-clean-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("c.gpx");
        // 5 fixes walking north, 80 fixes scattered around 50.01/8.0 a minute apart, 5 walking on.
        let mut pts = Vec::new();
        for i in 0..5 {
            pts.push((50.0 + i as f64 * 0.002, 8.0));
        }
        for i in 0..80 {
            let d = [0.0001, -0.0002, 0.00015, -0.00005][i % 4];
            pts.push((50.01 + d, 8.0 - d));
        }
        for i in 1..=5 {
            pts.push((50.01 + i as f64 * 0.002, 8.0));
        }
        let body: String = pts
            .iter()
            .enumerate()
            .map(|(i, (lat, lon))| {
                let t = chrono_like(i as i64 * 60);
                format!(r#"<trkpt lat="{lat}" lon="{lon}"><ele>{}</ele><time>{t}</time></trkpt>"#, 100 + i % 3)
            })
            .collect();
        std::fs::write(&gpx, format!("<gpx><trk><trkseg>{body}</trkseg></trk></gpx>")).unwrap();

        let mut conn = catalog::open(&dir.join("c.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let id = catalog::list_tracks(&conn).unwrap()[0].id;
        let before = catalog::load_track_data(&conn, id).unwrap();

        let o = StayOptions { radius_m: 50.0, min_duration_s: 600.0 };
        let removed = clean_up(&mut conn, id, Some(o), false, false).unwrap();
        let after = catalog::load_track_data(&conn, id).unwrap();
        assert_eq!(removed, before.coords.len() - after.coords.len());
        assert!(removed >= 76, "removed {removed}");
        assert_eq!(after.time.first(), before.time.first());
        assert_eq!(after.time.last(), before.time.last());
        // Arrival and departure share the spot.
        let spot = after.coords.iter().position(|c| (c.0 - 50.01).abs() < 0.0002).unwrap();
        assert_eq!(after.coords[spot], after.coords[spot + 1]);
        assert_eq!(after.time[spot + 1] - after.time[spot], before.time[spot + removed + 1] - before.time[spot]);
        assert!(catalog::list_tracks(&conn).unwrap()[0].edited);
        // Nothing left to clean.
        assert_eq!(clean_up(&mut conn, id, Some(o), true, false).unwrap(), 0);
        let file_id = catalog::list_tracks(&conn).unwrap()[0].file_id;
        assert_eq!(undo::last_edit(&conn, file_id).unwrap().unwrap().label, format!("Clean up ({removed} points)"));
        undo::undo(&mut conn, file_id).unwrap();
        let undone = catalog::load_track_data(&conn, id).unwrap();
        assert_eq!((undone.coords, undone.time), (before.coords, before.time));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clean_up_drops_fixes_at_a_zero_coordinate() {
        let dir = std::env::temp_dir().join(format!("gtm-zero-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("z.gpx");
        let pts = [(50.0, 8.0), (50.001, 8.0), (0.0, 8.0), (50.002, 0.0), (50.003, 0.00001), (50.004, 8.0)];
        let body: String = pts.iter().map(|(lat, lon)| format!(r#"<trkpt lat="{lat}" lon="{lon}"/>"#)).collect();
        std::fs::write(&gpx, format!("<gpx><trk><trkseg>{body}</trkseg></trk></gpx>")).unwrap();

        let mut conn = catalog::open(&dir.join("z.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let id = catalog::list_tracks(&conn).unwrap()[0].id;

        assert_eq!(clean_up(&mut conn, id, None, true, false).unwrap(), 2);
        let after = catalog::load_track_data(&conn, id).unwrap();
        assert_eq!(after.coords, vec![(50.0, 8.0), (50.001, 8.0), (50.003, 0.00001), (50.004, 8.0)]);
        assert_eq!(clean_up(&mut conn, id, None, true, false).unwrap(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clean_up_drops_spikes_and_zero_fixes_in_one_edit() {
        let dir = std::env::temp_dir().join(format!("gtm-spike-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("s.gpx");
        let mut pts: Vec<(f64, f64, &str)> = (0..30).map(|i| (46.7 + i as f64 * 0.0001, 7.6, "2012-10-06T15:42:57Z")).collect();
        pts.insert(10, (0.0019, 0.008, "1957-03-29T15:53:36Z"));
        pts.insert(11, (0.0, 7.6, "2012-10-06T15:42:57Z"));
        pts.insert(12, (1.815, 13.027, "2031-01-01T00:00:00Z"));
        let body: String =
            pts.iter().map(|(lat, lon, t)| format!(r#"<trkpt lat="{lat}" lon="{lon}"><time>{t}</time></trkpt>"#)).collect();
        std::fs::write(&gpx, format!("<gpx><trk><trkseg>{body}</trkseg></trk></gpx>")).unwrap();

        let mut conn = catalog::open(&dir.join("s.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let id = catalog::list_tracks(&conn).unwrap()[0].id;
        let before = catalog::load_track_data(&conn, id).unwrap();

        assert_eq!(clean_up(&mut conn, id, None, false, true).unwrap(), 3, "the zero fix is a spike too");
        let after = catalog::load_track_data(&conn, id).unwrap();
        assert!(after.coords.iter().all(|c| c.0 > 46.0));
        let file_id = catalog::list_tracks(&conn).unwrap()[0].file_id;
        undo::undo(&mut conn, file_id).unwrap();
        assert_eq!(catalog::load_track_data(&conn, id).unwrap().coords, before.coords);
        assert_eq!(clean_up(&mut conn, id, None, true, true).unwrap(), 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clean_up_collapses_stays_and_drops_zero_fixes_in_one_edit() {
        let dir = std::env::temp_dir().join(format!("gtm-both-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("b.gpx");
        // Walk, a 40-minute stay with 15 zero fixes in its middle, walk on, one more zero fix.
        let mut pts = Vec::new();
        for i in 0..5 {
            pts.push((50.0 + i as f64 * 0.002, 8.0));
        }
        for i in 0..40 {
            let d = [0.0001, -0.0002, 0.00015, -0.00005][i % 4];
            pts.push((50.01 + d, 8.0 - d));
            if i == 20 {
                pts.extend(std::iter::repeat_n((0.0, 8.0), 15));
            }
        }
        for i in 1..=5 {
            pts.push((50.01 + i as f64 * 0.002, 8.0));
        }
        pts.push((50.03, 0.0));
        pts.push((50.031, 8.0));
        let body: String = pts
            .iter()
            .enumerate()
            .map(|(i, (lat, lon))| format!(r#"<trkpt lat="{lat}" lon="{lon}"><time>{}</time></trkpt>"#, chrono_like(i as i64 * 60)))
            .collect();
        std::fs::write(&gpx, format!("<gpx><trk><trkseg>{body}</trkseg></trk></gpx>")).unwrap();

        let mut conn = catalog::open(&dir.join("b.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let id = catalog::list_tracks(&conn).unwrap()[0].id;
        let before = catalog::load_track_data(&conn, id).unwrap();

        let o = StayOptions { radius_m: 50.0, min_duration_s: 600.0 };
        let removed = clean_up(&mut conn, id, Some(o), true, false).unwrap();
        let after = catalog::load_track_data(&conn, id).unwrap();
        assert_eq!(removed, before.coords.len() - after.coords.len());
        assert!(!after.coords.iter().any(|&c| is_zero_fix(c)));
        // One stay across the zero fixes: only its arrival and departure are left at the spot.
        assert_eq!(after.coords.iter().filter(|c| (c.0 - 50.01).abs() < 0.0003 && c.1 > 7.9).count(), 2);

        let file_id = catalog::list_tracks(&conn).unwrap()[0].file_id;
        undo::undo(&mut conn, file_id).unwrap();
        let undone = catalog::load_track_data(&conn, id).unwrap();
        assert_eq!((undone.coords, undone.time), (before.coords, before.time));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clean_up_keeps_at_least_two_points() {
        let dir = std::env::temp_dir().join(format!("gtm-two-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("t.gpx");
        let body: String = [(50.0, 8.0), (0.0, 8.0), (50.0, 0.0), (50.001, 8.0)]
            .iter()
            .map(|(lat, lon)| format!(r#"<trkpt lat="{lat}" lon="{lon}"/>"#))
            .collect();
        std::fs::write(&gpx, format!("<gpx><trk><trkseg>{body}</trkseg></trk></gpx>")).unwrap();
        let mut conn = catalog::open(&dir.join("t.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let id = catalog::list_tracks(&conn).unwrap()[0].id;
        assert_eq!(clean_up(&mut conn, id, None, true, false).unwrap(), 2);

        std::fs::write(&gpx, format!("<gpx><trk><trkseg>{}</trkseg></trk></gpx>", r#"<trkpt lat="50" lon="8"/><trkpt lat="0" lon="8"/><trkpt lat="0" lon="8"/>"#)).unwrap();
        let mut conn = catalog::open(&dir.join("t2.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let id = catalog::list_tracks(&conn).unwrap()[0].id;
        assert!(clean_up(&mut conn, id, None, true, false).is_err());
        assert_eq!(catalog::load_track_data(&conn, id).unwrap().coords.len(), 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edits_are_undone_newest_first() {
        let dir = std::env::temp_dir().join(format!("gtm-undo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("u.gpx");
        let pts: String = (0..10)
            .map(|i| {
                format!(r#"<trkpt lat="50.{i:03}" lon="8.0"><time>2024-01-01T10:00:{i:02}Z</time><speed>{i}.5</speed></trkpt>"#)
            })
            .collect();
        std::fs::write(&gpx, format!("<gpx><trk><name>U</name><trkseg>{pts}</trkseg></trk></gpx>")).unwrap();

        let mut conn = catalog::open(&dir.join("c.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let t = &catalog::list_tracks(&conn).unwrap()[0];
        let (id, file_id) = (t.id, t.file_id);
        assert!(undo::last_edit(&conn, file_id).unwrap().is_none());

        let part = split(&mut conn, id, 6).unwrap();
        delete_points(&mut conn, part, &[0]).unwrap();
        delete_points(&mut conn, id, &[1, 2]).unwrap();
        let last = undo::last_edit(&conn, file_id).unwrap().unwrap();
        assert_eq!((last.label.as_str(), last.track_id), ("Delete 2 points", id));

        assert_eq!(catalog::load_track_data(&conn, id).unwrap().speed, vec![0.5, 3.5, 4.5, 5.5]);
        let r = undo::undo(&mut conn, file_id).unwrap();
        assert_eq!((r.restored, r.removed), (vec![id], None));
        let restored = catalog::load_track_data(&conn, id).unwrap();
        assert_eq!(restored.coords.len(), 6);
        assert_eq!(restored.speed, vec![0.5, 1.5, 2.5, 3.5, 4.5, 5.5], "undo restores the recorded speed");
        assert_eq!(undo::last_edit(&conn, file_id).unwrap().unwrap().track_id, part);
        undo::undo(&mut conn, file_id).unwrap();
        assert_eq!(catalog::load_track_data(&conn, part).unwrap().coords.len(), 4);
        let r = undo::undo(&mut conn, file_id).unwrap();
        assert_eq!(r.removed, Some(part));
        let tracks = catalog::list_tracks(&conn).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!((tracks[0].point_count, tracks[0].edited), (10, false));
        assert!(undo::undo(&mut conn, file_id).is_err());

        // Re-reading the file forgets the history: saved points would not match it.
        delete_points(&mut conn, id, &[0]).unwrap();
        assert!(undo::last_edit(&conn, file_id).unwrap().is_some());
        revert_file(&mut conn, file_id).unwrap();
        assert!(undo::last_edit(&conn, file_id).unwrap().is_none());

        // A recorded list that does not fit the points loads as unknown instead of breaking callers.
        conn.execute("UPDATE track_data SET speed = x'0000803F' WHERE track_id = ?1", [id]).unwrap();
        let d = catalog::load_track_data(&conn, id).unwrap();
        assert!(d.speed.len() == d.coords.len() && d.speed.iter().all(|v| v.is_nan()));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `2024-01-01T10:00:00Z` plus `s` seconds (under a day).
    fn chrono_like(s: i64) -> String {
        format!("2024-01-01T{:02}:{:02}:{:02}Z", 10 + s / 3600, s / 60 % 60, s % 60)
    }

    #[test]
    fn segments_extract_join_and_waypoints() {
        let dir = std::env::temp_dir().join(format!("gtm-seg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("s.gpx");
        let seg = |k: usize| -> String {
            let pts: String = (0..3)
                .map(|i| {
                    let n = k * 3 + i;
                    format!(r#"<trkpt lat="50.{n:03}" lon="8.0"><time>2024-01-01T10:00:{n:02}Z</time></trkpt>"#)
                })
                .collect();
            format!("<trkseg>{pts}</trkseg>")
        };
        let body = format!(
            r#"<gpx><wpt lat="50.1" lon="8.1"><name>Hut</name></wpt><trk>{}{}{}</trk></gpx>"#,
            seg(0),
            seg(1),
            seg(2)
        );
        std::fs::write(&gpx, body).unwrap();

        let mut conn = catalog::open(&dir.join("c.sqlite")).unwrap();
        let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap();
        catalog::write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let t = &catalog::list_tracks(&conn).unwrap()[0];
        let (id, file_id) = (t.id, t.file_id);
        let wps = catalog::list_file_waypoints(&conn, file_id).unwrap();
        assert_eq!((wps.len(), wps[0].name.as_deref(), wps[0].kind.as_str()), (1, Some("Hut"), "waypoint"));
        assert_eq!(catalog::load_track_data(&conn, id).unwrap().file_seg_starts, vec![0, 3, 6]);

        let middle = extract(&mut conn, id, 3, 6).unwrap();
        let rest = catalog::load_track_data(&conn, id).unwrap();
        assert_eq!((rest.coords.len(), rest.file_seg_starts.clone()), (6, vec![0, 3]));
        assert_eq!(catalog::load_track_data(&conn, middle).unwrap().coords.len(), 3);

        join_segment(&mut conn, id, 3).unwrap();
        assert_eq!(catalog::load_track_data(&conn, id).unwrap().file_seg_starts, vec![0]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn point_table_ignores_segment_start_at_end() {
        // Catalogs written before file segments were normalised.
        let mut d = data(2, vec![0]);
        d.file_seg_starts = vec![0, 2];
        assert_eq!(point_table(&d).len(), 2 * POINT_COLUMNS);
    }

    #[test]
    fn point_table_flags_segment_starts() {
        let mut d = data(4, vec![0, 2]);
        d.time = vec![0, 10_000, 20_000, 30_000];
        // A glitch break at 3 that is not one of the file's segments.
        let mut glitch = data(4, vec![0, 3]);
        glitch.file_seg_starts = vec![0];
        let g: Vec<f64> = point_table(&glitch).chunks(POINT_COLUMNS).map(|r| r[4]).collect();
        assert_eq!(g, vec![1.0, 0.0, 0.0, 2.0]);
        let t = point_table(&d);
        let rows: Vec<&[f64]> = t.chunks(POINT_COLUMNS).collect();
        let flags: Vec<f64> = rows.iter().map(|r| r[4]).collect();
        assert_eq!(flags, vec![1.0, 0.0, 1.0, 0.0]);
        // 0.001° of latitude is ~111 m; the gap between segments is not counted.
        assert!((rows[1][5] - 111.2).abs() < 0.5);
        assert_eq!(rows[2][5], rows[1][5]);
        assert!((rows[1][6] - 11.12).abs() < 0.1, "speed over 10 s");
        assert!(rows[0][7].abs() < 1e-6, "heading north");
    }
}
