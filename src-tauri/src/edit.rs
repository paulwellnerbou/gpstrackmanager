//! Point editing. Edits change the catalog copy only; the original file stays untouched
//! and `revert_file` restores it.

use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::{Connection, Transaction, params};

use crate::catalog::{self, TrackData};
use crate::geo::{bearing_deg, haversine_m};
use crate::model::{NO_TIME, ParsedTrack, Points};
use crate::parse::Format;
use crate::process::{ProcessedTrack, process_file, process_track};

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
    for &s in &data.file_seg_starts {
        if let Some(row) = out.get_mut(s as usize * POINT_COLUMNS..) {
            row[4] = 1.0;
        }
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
    let tx = conn.transaction()?;
    write_geometry(&tx, track_id, &processed)?;
    tx.commit()?;
    Ok(())
}

/// Splits before point `at`; the original keeps `[0, at)`, a new track gets the rest.
/// Returns the new track's id.
pub fn split(conn: &mut Connection, track_id: i64, at: u32) -> Result<i64> {
    let n = catalog::load_track_data(conn, track_id)?.coords.len() as u32;
    if at == 0 || at >= n {
        bail!("Cannot split at the first or after the last point.");
    }
    extract(conn, track_id, at, n)
}

/// Moves points `[start, end)` into a new track (e.g. one segment); the original keeps
/// the rest. Returns the new track's id.
pub fn extract(conn: &mut Connection, track_id: i64, start: u32, end: u32) -> Result<i64> {
    let data = catalog::load_track_data(conn, track_id)?;
    let (n, start, end) = (data.coords.len(), start as usize, (end as usize).min(data.coords.len()));
    let rest: Vec<usize> = (0..start).chain(end..n).collect();
    if rest.is_empty() || end <= start {
        bail!("Nothing would remain in the original track.");
    }
    let first = process(subset(&data, &rest));
    let second = process(subset(&data, &(start..end).collect::<Vec<_>>()));

    let tx = conn.transaction()?;
    let (file_id, path, base): (i64, String, Option<String>) = tx.query_row(
        "SELECT t.file_id, f.path, COALESCE(t.title, t.name) FROM tracks t JOIN files f ON f.id = t.file_id
         WHERE t.id = ?1",
        [track_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let base = base.unwrap_or_else(|| {
        Path::new(&path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(path.clone())
    });
    tx.execute(
        "INSERT INTO tracks(file_id, idx, name, file_description, title, description, file_activity, activity,
           point_count, distance_m, edited)
         SELECT file_id, (SELECT MIN(0, MIN(idx)) FROM tracks WHERE file_id = ?2) - 1,
                name, file_description, ?3, description, file_activity, activity, 0, 0, 1
         FROM tracks WHERE id = ?1",
        params![track_id, file_id, format!("{base} (part 2)")],
    )?;
    let new_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO track_tags(track_id, tag_id) SELECT ?2, tag_id FROM track_tags WHERE track_id = ?1",
        params![track_id, new_id],
    )?;
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
    let tx = conn.transaction()?;
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
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM tracks WHERE file_id = ?1 AND idx < 0", [file_id])?;
    tx.execute("UPDATE tracks SET edited = 0 WHERE file_id = ?1", [file_id])?;
    tx.commit()?;
    catalog::write_import(conn, root_id, &imported)
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
