//! Finds stays: stretches where the logger lay still (often indoors) and its fixes jump
//! around one spot in a star instead of staying put.

use serde::Serialize;

use crate::geo::haversine_m;
use crate::model::NO_TIME;

/// Without timestamps a stay is recognised by its number of fixes instead of its duration.
const MIN_POINTS_WITHOUT_TIME: usize = 60;
/// Star spikes leave the radius for a few fixes and come back; a longer excursion is
/// real movement and ends the stay.
const MAX_EXCURSION_POINTS: usize = 10;
const MAX_EXCURSION_MS: i64 = 120_000;
/// After a rejected spot the search skips 1/256 of it: on a 2.4M-point archive this finds
/// exactly the stays a fix-by-fix search finds.
const REJECTED_STEP_DIVISOR: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct StayOptions {
    /// How far fixes may scatter around the spot.
    pub radius_m: f64,
    /// How long the logger must stay for the stretch to count.
    pub min_duration_s: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Stay {
    /// First and last point of the stay (inclusive); they keep the arrival and departure times.
    pub start: u32,
    pub end: u32,
    /// The spot: mean of the fixes within the radius, spikes left out.
    pub lat: f64,
    pub lon: f64,
    /// NaN when no fix of the stay has an elevation.
    pub ele: f32,
}

/// Stays within each of the given segments (start indices, ascending), so a stay never
/// bridges two segments.
pub fn find_stays(coords: &[(f64, f64)], ele: &[f32], time: &[i64], seg_starts: &[u32], o: StayOptions) -> Vec<Stay> {
    let n = coords.len();
    let mut out = Vec::new();
    for (k, &s) in seg_starts.iter().enumerate() {
        let e = seg_starts.get(k + 1).map_or(n, |&e| e as usize).min(n);
        let mut i = s as usize;
        while i + 1 < e {
            match grow(coords, ele, time, i, e, o) {
                Ok(stay) => {
                    i = stay.end as usize + 1;
                    out.push(stay);
                }
                Err(next) => i = next,
            }
        }
    }
    out
}

/// The stay starting at `start`, if one does: follows the fixes while they stay within
/// the radius of the running centre, tolerating short spikes outside. Otherwise returns
/// where to look next.
fn grow(coords: &[(f64, f64)], ele: &[f32], time: &[i64], start: usize, end: usize, o: StayOptions) -> Result<Stay, usize> {
    let (mut sum_lat, mut sum_lon, mut count) = (coords[start].0, coords[start].1, 1usize);
    let mut last_in = start;
    for j in start + 1..end {
        if haversine_m(sum_lat / count as f64, sum_lon / count as f64, coords[j].0, coords[j].1) <= o.radius_m {
            sum_lat += coords[j].0;
            sum_lon += coords[j].1;
            count += 1;
            last_in = j;
            continue;
        }
        let misses = j - last_in;
        let away_ms = if time[j] != NO_TIME && time[last_in] != NO_TIME { time[j] - time[last_in] } else { 0 };
        // A single miss is always tolerated: sparse loggers take minutes between fixes.
        if misses > MAX_EXCURSION_POINTS || (misses > 1 && away_ms > MAX_EXCURSION_MS) {
            break;
        }
    }
    if count < 3 {
        return Err(start + 1);
    }
    settle(coords, ele, time, start, last_in, (sum_lat / count as f64, sum_lon / count as f64), o)
}

/// The running centre lags behind on arrival, so the approach walk is followed into the
/// stay. Re-centres on the fixes near the provisional centre and trims both ends to the
/// final spot's radius.
///
/// A spot held too briefly is rejected and the search moves on by a small fraction of
/// the stretch scanned for it. Retrying from every fix would be quadratic in long stays (25 s for a day-long
/// stay with a minimum above a day); jumping further misses stays, because a start
/// closer to the spot drags the centre less and can find a longer one.
fn settle(
    coords: &[(f64, f64)],
    ele: &[f32],
    time: &[i64],
    start: usize,
    end: usize,
    centre: (f64, f64),
    o: StayOptions,
) -> Result<Stay, usize> {
    let near = |c: (f64, f64), i: usize| haversine_m(c.0, c.1, coords[i].0, coords[i].1) <= o.radius_m;
    let (mut sum_lat, mut sum_lon, mut count) = (0f64, 0f64, 0usize);
    let (mut sum_ele, mut ele_count) = (0f64, 0usize);
    for i in (start..=end).filter(|&i| near(centre, i)) {
        sum_lat += coords[i].0;
        sum_lon += coords[i].1;
        count += 1;
        if !ele[i].is_nan() {
            sum_ele += ele[i] as f64;
            ele_count += 1;
        }
    }
    if count < 3 {
        return Err(start + 1);
    }
    let spot = (sum_lat / count as f64, sum_lon / count as f64);
    let Some(first) = (start..=end).find(|&i| near(spot, i)) else { return Err(start + 1) };
    let last = (first..=end).rev().find(|&i| near(spot, i)).unwrap_or(first);
    let timed = time[first] != NO_TIME && time[last] != NO_TIME;
    let long_enough = if timed {
        (time[last] - time[first]) as f64 / 1000.0 >= o.min_duration_s
    } else {
        last - first + 1 >= MIN_POINTS_WITHOUT_TIME
    };
    if !long_enough || last <= first + 1 {
        return Err(start + ((end - start) / REJECTED_STEP_DIVISOR).max(1));
    }
    Ok(Stay {
        start: first as u32,
        end: last as u32,
        lat: spot.0,
        lon: spot.1,
        ele: if ele_count > 0 { (sum_ele / ele_count as f64) as f32 } else { f32::NAN },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTS: StayOptions = StayOptions { radius_m: 50.0, min_duration_s: 600.0 };
    /// ~1.1 m of latitude.
    const M: f64 = 0.00001;

    struct Track {
        coords: Vec<(f64, f64)>,
        time: Vec<i64>,
    }

    impl Track {
        fn new() -> Self {
            Track { coords: vec![], time: vec![] }
        }
        fn push(&mut self, lat: f64, lon: f64) {
            self.time.push(self.time.len() as i64 * 10_000);
            self.coords.push((lat, lon));
        }
        fn walk(&mut self, from: f64, steps: usize) -> f64 {
            // 10 s per fix at ~1.5 m/s.
            for k in 0..steps {
                self.push(from + k as f64 * 14.0 * M, 8.0);
            }
            from + steps as f64 * 14.0 * M
        }
        fn stays(&self, o: StayOptions) -> Vec<Stay> {
            let ele = vec![f32::NAN; self.coords.len()];
            find_stays(&self.coords, &ele, &self.time, &[0], o)
        }
    }

    /// Deterministic scatter in a ±`r` metre box, with a spike every 7th fix.
    fn jitter(t: &mut Track, lat: f64, fixes: usize, r: f64) {
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0
        };
        for k in 0..fixes {
            let spike = if k % 7 == 3 { 4.0 } else { 1.0 };
            t.push(lat + rnd() * r * spike * M, 8.0 + rnd() * r * spike * M * 1.5);
        }
    }

    #[test]
    fn star_around_a_building_is_one_stay() {
        let mut t = Track::new();
        let lat = t.walk(50.0, 30);
        let first = t.coords.len();
        jitter(&mut t, lat, 360, 25.0); // one hour
        let last = t.coords.len() - 1;
        t.walk(lat + 100.0 * M, 30);

        let stays = t.stays(OPTS);
        assert_eq!(stays.len(), 1, "{stays:?}");
        let s = &stays[0];
        // The last few metres of the approach are within the radius and join the stay.
        assert!((s.start as usize).abs_diff(first) <= 4, "start {} vs {first}", s.start);
        assert!((s.end as usize).abs_diff(last) <= 8, "end {} vs {last}", s.end);
        assert!(haversine_m(s.lat, s.lon, lat, 8.0) < 15.0);
    }

    #[test]
    fn walking_and_short_stops_are_not_stays() {
        let mut t = Track::new();
        let lat = t.walk(50.0, 200);
        jitter(&mut t, lat, 30, 10.0); // five minutes
        t.walk(lat, 200);
        assert!(t.stays(OPTS).is_empty());
    }

    #[test]
    fn a_real_excursion_ends_the_stay() {
        let mut t = Track::new();
        jitter(&mut t, 50.0, 120, 10.0);
        let away = t.coords.len();
        // Five minutes 500 m away, then back.
        for _ in 0..30 {
            t.push(50.0 + 500.0 * M, 8.0);
        }
        jitter(&mut t, 50.0, 120, 10.0);
        let stays = t.stays(OPTS);
        assert_eq!(stays.len(), 2, "{stays:?}");
        assert!((stays[0].end as usize) < away && stays[1].start as usize >= away + 30);
    }

    #[test]
    fn a_stay_shorter_than_the_minimum_is_rejected_quickly() {
        // 30 000 fixes (a bit over 3 days at 10 s) against a 4-day minimum: retrying
        // every start would take minutes here.
        let mut t = Track::new();
        jitter(&mut t, 50.0, 30_000, 10.0);
        let started = std::time::Instant::now();
        assert!(t.stays(StayOptions { radius_m: 50.0, min_duration_s: 4.0 * 86_400.0 }).is_empty());
        assert!(started.elapsed().as_secs() < 5, "took {:?}", started.elapsed());
    }

    #[test]
    fn without_timestamps_fixes_are_counted() {
        let mut t = Track::new();
        jitter(&mut t, 50.0, 80, 10.0);
        let ele = vec![f32::NAN; t.coords.len()];
        let no_time = vec![NO_TIME; t.coords.len()];
        assert_eq!(find_stays(&t.coords, &ele, &no_time, &[0], OPTS).len(), 1);
        assert!(find_stays(&t.coords[..40], &ele[..40], &no_time[..40], &[0], OPTS).is_empty());
    }

    #[test]
    fn stays_do_not_bridge_segments() {
        let mut t = Track::new();
        jitter(&mut t, 50.0, 140, 10.0);
        let ele = vec![f32::NAN; t.coords.len()];
        let stays = find_stays(&t.coords, &ele, &t.time, &[0, 70], OPTS);
        assert_eq!(stays.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>(), vec![(0, 69), (70, 139)]);
    }
}
