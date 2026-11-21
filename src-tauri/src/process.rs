use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::codec;
use crate::geo::{haversine_m, pixel_size_at_zoom};
use crate::lod::compute_importance;
use crate::model::{NO_TIME, ParsedTrack, Waypoint};
use crate::parse::{self, Format};
use crate::places::{self, TrackPlace};

/// The library overview is drawn from a pre-thinned copy so showing every track at once
/// never touches the full point data.
const OVERVIEW_ZOOM: f64 = 10.0;

/// Bump when parsing or processing changes so the next scan re-reads every file.
pub const PARSER_VERSION: i64 = 2;

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
    pub places: Vec<TrackPlace>,
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
fn plausible_step(d: f64, t0: i64, t1: i64) -> bool {
    const MAX_SPEED_MS: f64 = 300.0;
    const GAP_S: f64 = 300.0;
    const GAP_M: f64 = 2_000.0;
    if t0 == NO_TIME || t1 == NO_TIME {
        return d < 50_000.0;
    }
    let dt = ((t1 - t0).abs() as f64 / 1000.0).max(1.0);
    d / dt < MAX_SPEED_MS && !(dt > GAP_S && d > GAP_M)
}

/// Starts a new segment at every discontinuity, so glitches and gaps are not drawn as
/// long straight lines and do not count as distance. The points themselves are kept.
fn split_at_glitches(p: &mut crate::model::Points) {
    let mut starts = Vec::with_capacity(p.seg_starts.len());
    for (s, e) in p.segments() {
        starts.push(s as u32);
        for i in s + 1..e {
            let d = haversine_m(p.lat[i - 1], p.lon[i - 1], p.lat[i], p.lon[i]);
            if !plausible_step(d, p.time[i - 1], p.time[i]) {
                starts.push(i as u32);
            }
        }
    }
    p.seg_starts = starts;
}

pub fn process_track(mut t: ParsedTrack) -> ProcessedTrack {
    let file_segs = t.points.seg_starts.clone();
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

    // The overview is detailed enough for town-level places and much shorter than the track.
    let places = places::index().track_places(&codec::decode_coords(&overview));
    let blobs = TrackBlobs {
        coords: codec::encode_coords(&p.lat, &p.lon),
        ele: codec::encode_f32(&p.ele),
        time: codec::encode_i64(&p.time),
        importance: codec::encode_f32(&importance),
        segs: codec::encode_u32(&p.seg_starts),
        file_segs: codec::encode_u32(&file_segs),
        overview,
    };
    ProcessedTrack { name: t.name, description: t.description, activity: t.activity, stats, blobs, places }
}
