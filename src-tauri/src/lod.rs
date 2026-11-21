//! Level of detail via Douglas-Peucker "importance": each point stores the tolerance
//! at which it would be dropped. Any zoom level then is a single filter pass
//! (`importance >= tolerance`), with no per-zoom copies of the track.

use crate::geo::mercator;

pub fn compute_importance(lat: &[f64], lon: &[f64], segments: &[(usize, usize)]) -> Vec<f32> {
    let n = lat.len();
    let mut xy = Vec::with_capacity(n);
    for i in 0..n {
        xy.push(mercator(lat[i], lon[i]));
    }
    let mut imp = vec![0f32; n];
    let mut stack: Vec<(usize, usize, f32)> = Vec::new();
    for &(start, end) in segments {
        let last = end - 1;
        imp[start] = f32::INFINITY;
        imp[last] = f32::INFINITY;
        // Iterative: recursion would overflow on tracks with a million points.
        stack.push((start, last, f32::INFINITY));
        while let Some((a, b, parent)) = stack.pop() {
            if b <= a + 1 {
                continue;
            }
            let (mut best, mut best_d) = (a + 1, -1.0f64);
            let mid = (a + b) / 2;
            for k in a + 1..b {
                let d = dist_to_segment(xy[k], xy[a], xy[b]);
                // Ties go to the point nearest the middle: on runs of identical fixes
                // (a logger lying still) always taking the first would make this O(n²).
                if d > best_d || (d == best_d && k.abs_diff(mid) < best.abs_diff(mid)) {
                    best_d = d;
                    best = k;
                }
            }
            if best_d <= 0.0 {
                // Everything in between lies on the line and keeps importance 0.
                continue;
            }
            // Clamping to the parent keeps the hierarchy monotone: a point never
            // survives a tolerance that already removed the point that split it off.
            let v = (best_d as f32).min(parent);
            imp[best] = v;
            stack.push((a, best, v));
            stack.push((best, b, v));
        }
    }
    imp
}

fn dist_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_always_kept_and_collinear_points_dropped() {
        let lat = [0.0, 0.0, 0.0, 1.0, 0.0];
        let lon = [0.0, 1.0, 2.0, 3.0, 4.0];
        let imp = compute_importance(&lat, &lon, &[(0, 5)]);
        assert!(imp[0].is_infinite() && imp[4].is_infinite());
        assert!(imp[3] > 0.0, "the peak matters");
        assert!(imp[1] < 1e-9, "collinear point is irrelevant");
    }

    #[test]
    fn stationary_runs_are_fast() {
        // A walk in, 200k identical fixes (a logger lying still), a walk out. Every interior
        // fix is equally far from the line, which made each split peel off a single point.
        let (walk, still) = (1000, 200_000);
        let mut lat = Vec::new();
        let mut lon = Vec::new();
        for i in 0..walk {
            lat.push(50.0 + i as f64 * 1e-4);
            lon.push(8.0);
        }
        lat.extend(std::iter::repeat_n(50.2, still));
        lon.extend(std::iter::repeat_n(8.1, still));
        for i in 0..walk {
            lat.push(50.0 - i as f64 * 1e-4);
            lon.push(8.3);
        }
        let n = lat.len();
        let t = std::time::Instant::now();
        compute_importance(&lat, &lon, &[(0, n)]);
        assert!(t.elapsed().as_secs_f64() < 2.0, "took {:?}", t.elapsed());
    }
}
