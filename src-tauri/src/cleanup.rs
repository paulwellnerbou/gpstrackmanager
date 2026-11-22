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

/// Loggers write 0 for a coordinate they have no fix for. Exact zero only: a real fix
/// near the equator or the prime meridian lands on 0 only at the storage precision (~1 cm).
pub fn is_zero_fix((lat, lon): (f64, f64)) -> bool {
    lat == 0.0 || lon == 0.0
}

/// Far enough to be no GPS error, close enough to catch fixes decoded from garbage, which
/// land anywhere on the globe (often near 0/0).
const SPIKE_DISTANCE_M: f64 = 100_000.0;
const MAX_SPIKE_POINTS: usize = 10;
/// An airliner; a spike reached faster than this is no trip.
const MAX_TRIP_SPEED_MS: f64 = 300.0;
/// Logged tracks step far less than this between fixes; drawn routes and sparse exports
/// may not, and their far points are real turns. Only judged without timestamps: in
/// misdecoded logs junk sits beside nearly every real fix, and times vouch for real trips.
const DENSE_STEP_M: f64 = 10_000.0;
/// Off a track's start or end, only one neighbour checks the time: garbage times decades
/// away would otherwise make any distance reachable.
const MAX_END_GAP_MS: i64 = 30 * 86_400_000;

/// Short runs of points (not in `skip`) that lie far from the track on both sides: off the
/// track and back, or off its start or end. A run whose timestamps are in order and make
/// the trip reachable is kept, e.g. a few fixes logged at the far end of a flight.
/// Runs never span segments.
pub fn find_spikes(coords: &[(f64, f64)], time: &[i64], seg_starts: &[u32], skip: &[bool]) -> Vec<u32> {
    let n = coords.len();
    let dist = |a: usize, b: usize| haversine_m(coords[a].0, coords[a].1, coords[b].0, coords[b].1);
    let far = |a: usize, run: &[usize]| run.iter().all(|&i| dist(a, i) > SPIKE_DISTANCE_M);
    let near = |a: usize, run: &[usize]| run.iter().all(|&i| dist(a, i) <= SPIKE_DISTANCE_M);
    let mut out = Vec::new();
    for (si, &s) in seg_starts.iter().enumerate() {
        let e = seg_starts.get(si + 1).map_or(n, |&e| e as usize).min(n);
        let kept: Vec<usize> = (s as usize..e).filter(|&i| !skip[i]).collect();
        let m = kept.len();
        let timeless = kept.iter().all(|&i| time[i] == NO_TIME);
        // Points taken as part of the track; the last one anchors the next run.
        let mut track: Vec<usize> = Vec::new();
        let mut k = 0;
        while k < m {
            let prev = track.last().copied();
            let mut spike_end = None;
            for j in k + 1..=(k + MAX_SPIKE_POINTS).min(m) {
                let run = &kept[k..j];
                if prev.is_some_and(|a| dist(a, run[run.len() - 1]) <= SPIKE_DISTANCE_M) {
                    break;
                }
                let next = kept.get(j).copied();
                let anchored = match (prev, next) {
                    (Some(a), Some(b)) => {
                        let dense = !timeless
                            || track.iter().rev().nth(1).is_some_and(|&c| dist(a, c) <= DENSE_STEP_M)
                            || kept.get(j + 1).is_some_and(|&c| dist(b, c) <= DENSE_STEP_M);
                        dense && far(a, run) && far(b, run) && {
                            let gap = dist(a, b);
                            run.iter().all(|&i| dist(a, i) > gap && dist(b, i) > gap)
                        }
                    }
                    // At the ends either side could be the odd one out: the track must go on
                    // beside the one neighbour for longer than any spike. At the start that is
                    // judged on all points, as garbage can form a track of its own that real
                    // fixes are interleaved with; at the end junk is already left out of `track`.
                    (None, Some(b)) => far(b, run) && kept.get(j..j + MAX_SPIKE_POINTS).is_some_and(|t| near(b, t)),
                    (Some(a), None) => {
                        far(a, run) && track.len() >= MAX_SPIKE_POINTS && near(a, &track[track.len() - MAX_SPIKE_POINTS..])
                    }
                    (None, None) => false,
                };
                if anchored && !reachable(coords, time, prev, run, next) {
                    spike_end = Some(j);
                    break;
                }
            }
            match spike_end {
                Some(j) => {
                    out.extend(kept[k..j].iter().map(|&i| i as u32));
                    k = j;
                }
                None => {
                    track.push(kept[k]);
                    k += 1;
                }
            }
        }
    }
    out
}

/// Whether the timestamps explain going from `prev` through `run` to `next`: all known,
/// in order, and at most airliner speed. Garbage fixes carry garbage times.
fn reachable(coords: &[(f64, f64)], time: &[i64], prev: Option<usize>, run: &[usize], next: Option<usize>) -> bool {
    let max_gap = if prev.is_none() || next.is_none() { MAX_END_GAP_MS } else { i64::MAX };
    let path: Vec<usize> = prev.into_iter().chain(run.iter().copied()).chain(next).collect();
    path.windows(2).all(|w| {
        let (a, b) = (w[0], w[1]);
        if time[a] == NO_TIME || time[b] == NO_TIME || time[b] < time[a] || time[b] - time[a] > max_gap {
            return false;
        }
        let d = haversine_m(coords[a].0, coords[a].1, coords[b].0, coords[b].1);
        d <= MAX_TRIP_SPEED_MS * (time[b] - time[a]) as f64 / 1000.0
    })
}

/// What a clean-up changes, found the same way for its preview and when it is applied.
#[derive(Debug, Default, Serialize)]
pub struct Plan {
    pub stays: Vec<Stay>,
    pub spikes: Vec<u32>,
}

/// Spikes are found without the zero fixes, stays without both: either inside a stay would
/// break it up as an excursion.
pub fn plan(
    coords: &[(f64, f64)],
    ele: &[f32],
    time: &[i64],
    seg_starts: &[u32],
    stays: Option<StayOptions>,
    drop_zero: bool,
    drop_spikes: bool,
) -> Plan {
    let mut skip: Vec<bool> = coords.iter().map(|&c| drop_zero && is_zero_fix(c)).collect();
    let spikes = if drop_spikes { find_spikes(coords, time, seg_starts, &skip) } else { Vec::new() };
    for &i in &spikes {
        skip[i as usize] = true;
    }
    let stays = stays.map_or_else(Vec::new, |o| find_stays_skipping(coords, ele, time, seg_starts, &skip, o));
    Plan { stays, spikes }
}

/// `find_stays` over the points not in `skip`, which would otherwise break stays up as
/// excursions. Indices refer to all points; skipped points inside a stay fall within it.
pub fn find_stays_skipping(
    coords: &[(f64, f64)],
    ele: &[f32],
    time: &[i64],
    seg_starts: &[u32],
    skip: &[bool],
    o: StayOptions,
) -> Vec<Stay> {
    if !skip.contains(&true) {
        return find_stays(coords, ele, time, seg_starts, o);
    }
    let kept: Vec<usize> = (0..coords.len()).filter(|&i| !skip[i]).collect();
    let c: Vec<_> = kept.iter().map(|&i| coords[i]).collect();
    let e: Vec<_> = kept.iter().map(|&i| ele[i]).collect();
    let t: Vec<_> = kept.iter().map(|&i| time[i]).collect();
    let seg: Vec<u32> = seg_starts.iter().map(|&s| kept.partition_point(|&i| i < s as usize) as u32).collect();
    let mut stays = find_stays(&c, &e, &t, &seg, o);
    for s in &mut stays {
        s.start = kept[s.start as usize] as u32;
        s.end = kept[s.end as usize] as u32;
    }
    stays
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
    fn zero_fixes_inside_a_stay_do_not_break_it() {
        let mut t = Track::new();
        jitter(&mut t, 50.0, 60, 10.0);
        let gap = t.coords.len();
        for _ in 0..15 {
            t.push(0.0, 8.0);
        }
        jitter(&mut t, 50.0, 60, 10.0);
        let ele = vec![f32::NAN; t.coords.len()];
        assert_eq!(t.stays(OPTS).len(), 0, "each half alone is too short");
        let skip: Vec<bool> = t.coords.iter().map(|&c| is_zero_fix(c)).collect();
        let stays = find_stays_skipping(&t.coords, &ele, &t.time, &[0], &skip, OPTS);
        assert_eq!(stays.len(), 1, "{stays:?}");
        assert!((stays[0].start as usize) < gap && stays[0].end as usize >= gap + 15);
        assert!(!is_zero_fix(t.coords[stays[0].start as usize]) && !is_zero_fix(t.coords[stays[0].end as usize]));
    }

    #[test]
    fn skipped_points_keep_segment_boundaries() {
        let mut t = Track::new();
        jitter(&mut t, 50.0, 140, 10.0);
        t.coords[70] = (0.0, 8.0);
        let ele = vec![f32::NAN; t.coords.len()];
        let skip: Vec<bool> = t.coords.iter().map(|&c| is_zero_fix(c)).collect();
        let stays = find_stays_skipping(&t.coords, &ele, &t.time, &[0, 70], &skip, OPTS);
        assert_eq!(stays.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>(), vec![(0, 69), (71, 139)]);
    }

    /// Fixes every 10 s along a line from `lat`, with the given junk spliced in after `at`.
    fn line_with(lat: f64, len: usize, at: usize, junk: &[((f64, f64), i64)]) -> (Vec<(f64, f64)>, Vec<i64>) {
        let (mut c, mut t) = (vec![], vec![]);
        for k in 0..len {
            c.push((lat + k as f64 * 14.0 * M, 8.0));
            t.push(k as i64 * 10_000);
            if k == at {
                for &(p, time) in junk {
                    c.push(p);
                    t.push(time);
                }
            }
        }
        (c, t)
    }

    fn spikes(c: &[(f64, f64)], t: &[i64]) -> Vec<u32> {
        find_spikes(c, t, &[0], &vec![false; c.len()])
    }

    #[test]
    fn garbage_fixes_off_the_track_are_spikes() {
        // As decoded from a misread log: near 0/0, with times decades off.
        let junk = [((0.0019, 0.008), -1_000_000_000_000), ((1.815, 13.027), 2_000_000_000_000), ((1e-9, 7.7e-7), NO_TIME)];
        let (c, t) = line_with(46.7, 50, 20, &junk);
        assert_eq!(spikes(&c, &t), vec![21, 22, 23]);
    }

    #[test]
    fn a_far_fix_with_a_plausible_time_is_still_a_spike_when_too_fast() {
        let (c, t) = line_with(46.7, 50, 20, &[((46.7, 12.0), 205_000)]);
        assert_eq!(spikes(&c, &t), vec![21]);
    }

    #[test]
    fn a_short_recording_at_the_far_end_of_a_trip_is_kept() {
        // Logged for a minute in London a day later, back home the next day.
        let mut c = vec![(50.1, 8.6), (50.1001, 8.6)];
        let mut t = vec![0, 10_000];
        for k in 0..6 {
            c.push((51.5 + k as f64 * 0.0001, -0.12));
            t.push(86_400_000 + k * 10_000);
        }
        c.extend([(50.1, 8.6), (50.1001, 8.6)]);
        t.extend([2 * 86_400_000, 2 * 86_400_000 + 10_000]);
        assert!(spikes(&c, &t).is_empty());
        // Without times nothing vouches for the trip.
        assert_eq!(spikes(&c, &vec![NO_TIME; c.len()]), (2..8).collect::<Vec<_>>());
    }

    #[test]
    fn moving_on_to_a_new_region_is_no_spike() {
        // Off one week in Thun, on again in Frankfurt; a junk fix right at the change.
        let mut c: Vec<_> = (0..15).map(|k| (46.75 + k as f64 * 0.0001, 7.63)).collect();
        let mut t: Vec<_> = (0..15).map(|k| k * 2_000).collect();
        c.push((0.0007, 3.25));
        t.push(5);
        for k in 0..20 {
            c.push((50.15 + k as f64 * 0.0001, 8.42));
            t.push(7 * 86_400_000 + k * 2_000);
        }
        assert_eq!(spikes(&c, &t), vec![15]);
    }

    #[test]
    fn spikes_at_the_start_and_end_of_a_track() {
        let (mut c, mut t) = line_with(46.7, 30, 99, &[]);
        c.insert(0, (0.001, 0.002));
        t.insert(0, NO_TIME);
        c.push((2.0, 3.0));
        t.push(-5);
        assert_eq!(spikes(&c, &t), vec![0, 31]);
    }

    #[test]
    fn junk_with_far_off_times_at_the_ends_is_a_spike() {
        // Garbage times on the plausible side: before the first fix, after the last.
        let (mut c, mut t) = line_with(46.7, 30, 99, &[]);
        c.insert(0, (0.001, 0.002));
        t.insert(0, -1_000_000_000_000);
        c.push((0.0007, 3.25));
        t.push(4_000_000_000_000);
        assert_eq!(spikes(&c, &t), vec![0, 31]);
        // A recording a day after the track is a trip.
        t[31] = t[30] + 86_400_000;
        assert_eq!(spikes(&c, &t), vec![0]);
    }

    #[test]
    fn junk_at_the_end_after_more_junk_is_a_spike() {
        let (mut c, mut t) = line_with(46.7, 40, 99, &[]);
        for k in [32, 35, 38] {
            c.insert(k, (0.002 * k as f64, 0.003));
            t.insert(k, NO_TIME);
        }
        c.push((0.0006, 46.37));
        t.push(NO_TIME);
        assert_eq!(spikes(&c, &t), vec![32, 35, 38, 43]);
    }

    #[test]
    fn far_turns_of_a_sparse_route_are_kept() {
        // Frankfurt, Munich, Frankfurt without times, as drawn or exported.
        let c = [(50.1, 8.68), (48.14, 11.58), (50.1, 8.68)];
        assert!(spikes(&c, &[NO_TIME; 3]).is_empty());
        // There and back with a point every 200 km.
        let c: Vec<_> = (0..12).map(|k: usize| (40.0 + 1.8 * k.min(11 - k) as f64, 8.0)).collect();
        assert!(spikes(&c, &vec![NO_TIME; c.len()]).is_empty());
    }

    #[test]
    fn spikes_stay_within_segments_and_skip_points() {
        let (mut c, mut t) = line_with(46.7, 40, 19, &[((0.001, 0.002), NO_TIME)]);
        // The junk fix opens the second segment, right after the first one ends nearby.
        let seg = [0u32, 20];
        let skip = vec![false; c.len()];
        assert_eq!(find_spikes(&c, &t, &seg, &skip), vec![20]);
        // A skipped fix is never an anchor: next to it, the junk still has the track around it.
        c.insert(22, (0.0, 8.0));
        t.insert(22, NO_TIME);
        let mut skip = vec![false; c.len()];
        skip[22] = true;
        c.insert(23, (0.003, 0.001));
        t.insert(23, NO_TIME);
        skip.insert(23, false);
        assert_eq!(find_spikes(&c, &t, &[0], &skip), vec![20, 23]);
    }

    #[test]
    fn long_detours_are_not_spikes() {
        let junk: Vec<_> = (0..MAX_SPIKE_POINTS + 1).map(|k| ((0.01 * k as f64, 0.01), NO_TIME)).collect();
        let (c, t) = line_with(46.7, 50, 20, &junk);
        assert!(spikes(&c, &t).is_empty());
    }

    #[test]
    fn stays_are_found_without_the_spikes() {
        let mut t = Track::new();
        jitter(&mut t, 50.0, 60, 10.0);
        // Garbage times: a long excursion as far as the stay can tell.
        for k in 0..8 {
            t.coords.push((0.001, 0.002));
            t.time.push(2_000_000_000_000 + k);
        }
        jitter(&mut t, 50.0, 60, 10.0);
        let ele = vec![f32::NAN; t.coords.len()];
        let found = plan(&t.coords, &ele, &t.time, &[0], Some(OPTS), false, true);
        assert_eq!(found.spikes, (60..68).collect::<Vec<_>>());
        assert_eq!(found.stays.len(), 1, "{:?}", found.stays);
        assert!(plan(&t.coords, &ele, &t.time, &[0], Some(OPTS), false, false).stays.is_empty());
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
