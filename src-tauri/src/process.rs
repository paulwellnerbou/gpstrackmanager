use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::codec;
use crate::geo::{haversine_m, pixel_size_at_zoom};
use crate::lod::compute_importance;
use crate::model::{NO_TIME, ParsedTrack, Waypoint};
use crate::parse::{self, Format};

/// The library overview is drawn from a pre-thinned copy so showing every track at once
/// never touches the full point data.
const OVERVIEW_ZOOM: f64 = 10.0;

/// Bump when parsing or processing changes so the next scan re-reads every file.
pub const PARSER_VERSION: i64 = 3;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Stats {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub point_count: usize,
    pub distance_m: f64,
    pub bbox: Option<[f64; 4]>,
    pub min_ele: Option<f32>,
    pub max_ele: Option<f32>,
}

pub struct TrackBlobs {
    pub coords: Vec<u8>,
    pub ele: Vec<u8>,
    pub time: Vec<u8>,
    pub importance: Vec<u8>,
    /// All line breaks: the file's segments plus glitch and gap splits.
    pub segs: Vec<u8>,
    /// Segment starts as in the file (or as edited), without glitch splits.
    pub file_segs: Vec<u8>,
    pub overview: Vec<u8>,
}

pub struct ProcessedTrack {
    pub name: Option<String>,
    pub description: Option<String>,
    pub activity: Option<String>,
    pub stats: Stats,
    pub blobs: TrackBlobs,
}

pub struct ProcessedFile {
    pub tracks: Vec<ProcessedTrack>,
    pub waypoints: Vec<Waypoint>,
}

pub struct FileImport {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub hash: String,
    pub format: &'static str,
    pub result: Result<ProcessedFile, String>,
}

pub fn file_mtime(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub fn process_file(path: &Path, format: Format) -> FileImport {
    let path_str = path.to_string_lossy().into_owned();
    let meta = std::fs::metadata(path);
    let (size, mtime) = meta.as_ref().map(|m| (m.len() as i64, file_mtime(m))).unwrap_or((0, 0));
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return FileImport {
                path: path_str,
                size,
                mtime,
                hash: String::new(),
                format: format.as_str(),
                result: Err(e.to_string()),
            };
        }
    };
    let hash = hash_bytes(&bytes);
    let result = parse::parse(format, &bytes)
        .map(|file| ProcessedFile {
            tracks: file.tracks.into_iter().filter(|t| !t.points.is_empty()).map(process_track).collect(),
            waypoints: file.waypoints,
        })
        .map_err(|e| format!("{e:#}"));
    FileImport { path: path_str, size, mtime, hash, format: format.as_str(), result }
}

/// Whether two consecutive points belong to one continuous line. Not continuous: fix
/// glitches (faster than an airliner) and recording gaps (logger off, moved, on again).
/// A gap is a pause well beyond the logger's usual interval, so loggers that only record
/// every few minutes are not cut at every point.
fn plausible_step(d: f64, t0: i64, t1: i64, gap_s: f64) -> bool {
    const MAX_SPEED_MS: f64 = 300.0;
    const GAP_M: f64 = 2_000.0;
    if t0 == NO_TIME || t1 == NO_TIME {
        return d < 50_000.0;
    }
    let dt = ((t1 - t0).abs() as f64 / 1000.0).max(1.0);
    d / dt < MAX_SPEED_MS && !(dt > gap_s && d > GAP_M)
}

/// Gap threshold: 5 minutes, or ten times the median sampling interval if that is longer.
fn gap_threshold_s(time: &[i64]) -> f64 {
    let mut steps: Vec<i64> = time
        .windows(2)
        .filter(|w| w[0] != NO_TIME && w[1] != NO_TIME && w[1] > w[0])
        .map(|w| w[1] - w[0])
        .collect();
    if steps.is_empty() {
        return 300.0;
    }
    let mid = steps.len() / 2;
    let median_ms = *steps.select_nth_unstable(mid).1;
    (median_ms as f64 / 1000.0 * 10.0).max(300.0)
}

/// Starts a new segment at every discontinuity, so glitches and gaps are not drawn as
/// long straight lines and do not count as distance. The points themselves are kept.
fn split_at_glitches(p: &mut crate::model::Points) {
    let gap_s = gap_threshold_s(&p.time);
    let mut starts = Vec::with_capacity(p.seg_starts.len());
    for (s, e) in p.segments() {
        starts.push(s as u32);
        for i in s + 1..e {
            let d = haversine_m(p.lat[i - 1], p.lon[i - 1], p.lat[i], p.lon[i]);
            if !plausible_step(d, p.time[i - 1], p.time[i], gap_s) {
                starts.push(i as u32);
            }
        }
    }
    p.seg_starts = starts;
}

pub fn process_track(mut t: ParsedTrack) -> ProcessedTrack {
    // Normalised: empty segments (e.g. a trailing empty <trkseg>) are dropped.
    let file_segs: Vec<u32> = t.points.segments().iter().map(|&(s, _)| s as u32).collect();
    split_at_glitches(&mut t.points);
    let p = &t.points;
    let segments = p.segments();
    let importance = compute_importance(&p.lat, &p.lon, &segments);

    let mut stats = Stats { point_count: p.len(), ..Default::default() };
    let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for i in 0..p.len() {
        let e = p.ele[i];
        if !e.is_nan() {
            stats.min_ele = Some(stats.min_ele.map_or(e, |m| m.min(e)));
            stats.max_ele = Some(stats.max_ele.map_or(e, |m| m.max(e)));
        }
    }
    // Isolated points are almost always glitches; leaving them out keeps "zoom to track" useful.
    for &(s, e) in segments.iter().filter(|(s, e)| e - s > 1 || segments.len() == 1) {
        for i in s..e {
            bbox[0] = bbox[0].min(p.lat[i]);
            bbox[1] = bbox[1].min(p.lon[i]);
            bbox[2] = bbox[2].max(p.lat[i]);
            bbox[3] = bbox[3].max(p.lon[i]);
        }
    }
    if bbox[0] <= bbox[2] {
        stats.bbox = Some(bbox);
    }
    stats.start_time = p.time.iter().copied().find(|&t| t != NO_TIME);
    stats.end_time = p.time.iter().rev().copied().find(|&t| t != NO_TIME);

    let tol = pixel_size_at_zoom(OVERVIEW_ZOOM) as f32;
    let mut overview = Vec::new();
    for (si, &(s, e)) in segments.iter().enumerate() {
        if si > 0 {
            codec::push_break(&mut overview);
        }
        for i in s..e {
            if i > s {
                stats.distance_m += haversine_m(p.lat[i - 1], p.lon[i - 1], p.lat[i], p.lon[i]);
            }
            if importance[i] >= tol {
                codec::push_coord(&mut overview, p.lat[i], p.lon[i]);
            }
        }
    }

    let blobs = TrackBlobs {
        coords: codec::encode_coords(&p.lat, &p.lon),
        ele: codec::encode_f32(&p.ele),
        time: codec::encode_i64(&p.time),
        importance: codec::encode_f32(&importance),
        segs: codec::encode_u32(&p.seg_starts),
        file_segs: codec::encode_u32(&file_segs),
        overview,
    };
    ProcessedTrack { name: t.name, description: t.description, activity: t.activity, stats, blobs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Points;

    fn track(points: &[(f64, f64, i64)], segs: &[usize]) -> ParsedTrack {
        let mut p = Points::default();
        for (i, &(lat, lon, t)) in points.iter().enumerate() {
            if segs.contains(&i) {
                p.start_segment();
            }
            p.push(lat, lon, None, Some(t));
        }
        ParsedTrack { name: None, description: None, activity: None, points: p }
    }

    #[test]
    fn sparse_logger_is_not_cut_at_every_point() {
        // A car logged every 10 minutes, ~10 km apart.
        let pts: Vec<_> = (0..6).map(|i| (50.0 + i as f64 * 0.09, 8.0, i * 600_000)).collect();
        let t = process_track(track(&pts, &[]));
        assert!(t.stats.distance_m > 40_000.0, "distance {}", t.stats.distance_m);
        assert!(t.stats.bbox.is_some());
    }

    #[test]
    fn trailing_empty_segment_is_dropped_from_file_segments() {
        let mut t = track(&[(50.0, 8.0, 0), (50.001, 8.0, 1000)], &[]);
        t.points.start_segment();
        let processed = process_track(t);
        assert_eq!(codec::decode_u32(&processed.blobs.file_segs), vec![0]);
    }
}
