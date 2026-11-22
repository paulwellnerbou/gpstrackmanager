//! Point editing. Edits change the catalog copy only; the original file stays untouched
//! and `revert_file` restores it.

use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::{Connection, Transaction, params};

use crate::catalog::{self, TrackData};
use crate::cleanup::{self, StayOptions, is_zero_fix};
use crate::geo::{bearing_deg, haversine_m};
use crate::model::{NO_TIME, ParsedTrack, Points};
use crate::parse::Format;
use crate::process::{ProcessedTrack, process_file, process_track};
use crate::undo;

pub const POINT_COLUMNS: usize = 8;

/// Flat rows of `[lat, lon, ele, time_ms, segment_start, distance_m, speed_ms, bearing_deg]`.
/// NaN marks unknown values. `segment_start` is 1 where one of the file's segments begins,
/// 2 where the line is only broken at a glitch or recording gap, else 0.
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
        p.push(lat, lon, ele, time);
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
    let b = &t.blobs;
    tx.execute(
        "INSERT OR REPLACE INTO track_data(track_id, coords, ele, time, importance, segs, overview, file_segs)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![track_id, b.coords, b.ele, b.time, b.importance, b.segs, b.overview, b.file_segs],
    )?;
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
            .map(|i| format!(r#"<trkpt lat="50.{i:03}" lon="8.0"><time>2024-01-01T10:00:{i:02}Z</time></trkpt>"#))
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

        let r = undo::undo(&mut conn, file_id).unwrap();
        assert_eq!((r.restored, r.removed), (vec![id], None));
        assert_eq!(catalog::load_track_data(&conn, id).unwrap().coords.len(), 6);
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
