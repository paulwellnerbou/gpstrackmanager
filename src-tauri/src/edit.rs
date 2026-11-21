//! Point editing. Edits change the catalog copy only; the original file stays untouched
//! and `revert_file` restores it.

use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::{Connection, Transaction, params};

use crate::catalog::{self, TrackData};
use crate::model::{NO_TIME, ParsedTrack, Points};
use crate::parse::Format;
use crate::process::{ProcessedTrack, process_file, process_track};

/// Flat `[lat, lon, ele, time_ms, segment_start, …]`; NaN for unknown ele/time,
/// `segment_start` is 1 for the first point of a segment.
pub fn point_table(data: &TrackData) -> Vec<f64> {
    let mut seg = data.seg_starts.iter().peekable();
    let mut out = Vec::with_capacity(data.coords.len() * 5);
    for (i, &(lat, lon)) in data.coords.iter().enumerate() {
        let mut start = false;
        while seg.peek().is_some_and(|&&s| s as usize <= i) {
            start |= *seg.next().unwrap() as usize == i;
        }
        out.extend_from_slice(&[
            lat,
            lon,
            data.ele[i] as f64,
            if data.time[i] == NO_TIME { f64::NAN } else { data.time[i] as f64 },
            if start { 1.0 } else { 0.0 },
        ]);
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
        while si < data.seg_starts.len() && data.seg_starts[si] as usize <= i {
            if prev.is_none_or(|pv| data.seg_starts[si] as usize > pv) {
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
    process_track(ParsedTrack { name: None, description: None, points })
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
        "INSERT OR REPLACE INTO track_data(track_id, coords, ele, time, importance, segs, overview)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![track_id, b.coords, b.ele, b.time, b.importance, b.segs, b.overview],
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
    let data = catalog::load_track_data(conn, track_id)?;
    let (n, at) = (data.coords.len(), at as usize);
    if at == 0 || at >= n {
        bail!("Cannot split at the first or after the last point.");
    }
    let first = process(subset(&data, &(0..at).collect::<Vec<_>>()));
    let second = process(subset(&data, &(at..n).collect::<Vec<_>>()));

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
        "INSERT INTO tracks(file_id, idx, name, file_description, title, description, point_count, distance_m, edited)
         SELECT file_id, (SELECT MIN(0, MIN(idx)) FROM tracks WHERE file_id = ?2) - 1,
                name, file_description, ?3, description, 0, 0, 1
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
    fn point_table_flags_segment_starts() {
        let t = point_table(&data(4, vec![0, 2]));
        let flags: Vec<f64> = t.chunks(5).map(|r| r[4]).collect();
        assert_eq!(flags, vec![1.0, 0.0, 1.0, 0.0]);
    }
}
