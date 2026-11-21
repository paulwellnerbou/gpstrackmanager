use crate::catalog::TrackData;
use crate::geo::haversine_m;
use crate::model::NO_TIME;

fn segments(data: &TrackData) -> Vec<(usize, usize)> {
    let n = data.coords.len();
    let mut out = Vec::new();
    for (i, &s) in data.seg_starts.iter().enumerate() {
        let e = data.seg_starts.get(i + 1).map(|&e| e as usize).unwrap_or(n);
        if (s as usize) < e {
            out.push((s as usize, e));
        }
    }
    out
}

/// Points for display as flat `[lon, lat, …]` with NaN pairs as line breaks.
/// `tolerance` is in normalised mercator units (see `geo::pixel_size_at_zoom`);
/// `bbox` (`[min_lon, min_lat, max_lon, max_lat]`) drops lines that cannot touch the view.
/// A line is kept when its own bounding box overlaps the view, so a long straight stretch
/// crossing the screen survives even with both ends far outside.
pub fn select_points(data: &TrackData, tolerance: f32, bbox: Option<[f64; 4]>) -> Vec<f64> {
    let overlaps = |a: usize, b: usize| match bbox {
        None => true,
        Some([x0, y0, x1, y1]) => {
            let ((la, lo_a), (lb, lo_b)) = (data.coords[a], data.coords[b]);
            la.max(lb) >= y0 && la.min(lb) <= y1 && lo_a.max(lo_b) >= x0 && lo_a.min(lo_b) <= x1
        }
    };
    let mut out = Vec::new();
    let mut candidates = Vec::new();
    for (s, e) in segments(data) {
        candidates.clear();
        candidates.extend((s..e).filter(|&i| data.importance[i] >= tolerance));
        let mut last_emitted: Option<usize> = None;
        for (j, &i) in candidates.iter().enumerate() {
            let keep = overlaps(i, i)
                || (j > 0 && overlaps(candidates[j - 1], i))
                || (j + 1 < candidates.len() && overlaps(i, candidates[j + 1]));
            if !keep {
                continue;
            }
            let contiguous = last_emitted.is_some_and(|l| l + 1 == j);
            if !contiguous && !out.is_empty() {
                out.extend_from_slice(&[f64::NAN, f64::NAN]);
            }
            let (lat, lon) = data.coords[i];
            out.push(lon);
            out.push(lat);
            last_emitted = Some(j);
        }
    }
    out
}

/// Elevation profile as flat `[distance_m, ele, time_ms, lon, lat, …]` (time NaN if unknown).
/// Long tracks are reduced to the min and max elevation per distance bucket, which keeps
/// peaks and valleys visible however many points there are.
pub fn profile(data: &TrackData, buckets: usize) -> Vec<f64> {
    let n = data.coords.len();
    let mut dist = vec![0f64; n];
    for (s, e) in segments(data) {
        for i in s..e {
            dist[i] = if i == 0 {
                0.0
            } else if i == s {
                dist[i - 1]
            } else {
                let (a, b) = (data.coords[i - 1], data.coords[i]);
                dist[i - 1] + haversine_m(a.0, a.1, b.0, b.1)
            };
        }
    }
    let valid: Vec<usize> = (0..n).filter(|&i| !data.ele[i].is_nan()).collect();
    let emit = |out: &mut Vec<f64>, i: usize| {
        out.push(dist[i]);
        out.push(data.ele[i] as f64);
        out.push(if data.time[i] == NO_TIME { f64::NAN } else { data.time[i] as f64 });
        out.push(data.coords[i].1);
        out.push(data.coords[i].0);
    };
    let mut out = Vec::new();
    if valid.len() <= buckets * 2 {
        for &i in &valid {
            emit(&mut out, i);
        }
        return out;
    }
    let total = dist[n - 1].max(1.0);
    let mut k = 0;
    while k < valid.len() {
        let bucket = ((dist[valid[k]] / total) * buckets as f64) as usize;
        let (mut lo, mut hi) = (valid[k], valid[k]);
        while k < valid.len() && ((dist[valid[k]] / total) * buckets as f64) as usize == bucket {
            let i = valid[k];
            if data.ele[i] < data.ele[lo] {
                lo = i;
            }
            if data.ele[i] > data.ele[hi] {
                hi = i;
            }
            k += 1;
        }
        let (a, b) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        emit(&mut out, a);
        if b != a {
            emit(&mut out, b);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_long_line_crossing_the_view() {
        // Two vertices far left and right of a small view centred between them.
        let data = TrackData {
            coords: vec![(50.0, 7.0), (50.0, 9.0)],
            ele: vec![f32::NAN; 2],
            time: vec![crate::model::NO_TIME; 2],
            importance: vec![f32::INFINITY; 2],
            seg_starts: vec![0],
            file_seg_starts: vec![0],
        };
        let pts = select_points(&data, 0.0, Some([7.9, 49.9, 8.1, 50.1]));
        assert_eq!(pts.len(), 4, "both ends are kept so the line is drawn");
        assert!(select_points(&data, 0.0, Some([7.9, 51.0, 8.1, 51.2])).is_empty());
    }
}
