//! Finds tracks in different files that are the same recording (a logger's file and its
//! exports, a copy in another folder), and tracks that lie within a longer one (a day cut
//! from a multi-day dump). Same tracks are bundled: the list and map show one of them.
//! Contained tracks are only noted; bundling them would hide either the day or the dump.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use rusqlite::{Connection, TransactionBehavior, params};

use crate::codec;
use crate::geo::haversine_m;
use crate::model::NO_TIME;

/// Bump to recompute every track's matches after the rules below change.
pub const MATCH_VERSION: i64 = 1;

/// Share of each time span the other must cover to count as the same recording, or of
/// the shorter one's to count as lying within the longer.
const SPAN_SHARE: f64 = 0.9;
/// Positions sampled along a track to compare it with another.
const SAMPLES: usize = 24;
/// Share of samples that must lie on the other track.
const HIT_SHARE: f64 = 0.8;
const MIN_SAMPLES: usize = 3;
/// Two fixes of one moment, logged by different devices or rounded by an export.
const NEAR_M: f64 = 50.0;
/// Over a longer gap the other track's position at a sampled time is unknown.
const MAX_GAP_MS: i64 = 5 * 60_000;

/// What the catalog knows about a track without loading its points.
#[derive(Debug, Clone)]
pub struct Meta {
    pub id: i64,
    pub file_id: i64,
    pub start: Option<i64>,
    pub end: Option<i64>,
    /// min lat, min lon, max lat, max lon
    pub bbox: Option<[f64; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Same,
    /// The first track lies within the second.
    Within,
    /// The second track lies within the first.
    Encloses,
}

/// How two tracks might relate, judged by time span or, without times, by extent. Tracks
/// of one file never match: those are parts the user split, or separate tracks.
pub fn candidate(a: &Meta, b: &Meta) -> Option<Relation> {
    if a.file_id == b.file_id {
        return None;
    }
    match ((a.start, a.end), (b.start, b.end)) {
        ((Some(sa), Some(ea)), (Some(sb), Some(eb))) => {
            if ea <= sa || eb <= sb {
                return None;
            }
            let overlap = (ea.min(eb) - sa.max(sb)) as f64;
            if overlap <= 0.0 {
                return None;
            }
            let (fa, fb) = (overlap / (ea - sa) as f64, overlap / (eb - sb) as f64);
            match (fa >= SPAN_SHARE, fb >= SPAN_SHARE) {
                (true, true) => Some(Relation::Same),
                (true, false) => Some(Relation::Within),
                (false, true) => Some(Relation::Encloses),
                (false, false) => None,
            }
        }
        // A timed recording and an untimed route along the same roads are different things.
        ((None, None), (None, None)) => {
            let (a, b) = (a.bbox?, b.bbox?);
            let tol = |k: usize| 0.002f64.max(0.05 * (a[k + 2] - a[k]).max(b[k + 2] - b[k]));
            (0..4).all(|k| (a[k] - b[k]).abs() <= tol(k % 2)).then_some(Relation::Same)
        }
        _ => None,
    }
}

pub struct Points<'a> {
    pub coords: &'a [(f64, f64)],
    /// Unix ms, `NO_TIME` when unknown.
    pub time: &'a [i64],
}

/// Whether the points bear out a `candidate` relation.
pub fn confirm(rel: Relation, a: &Points, b: &Points) -> bool {
    let timed = a.time.iter().any(|&t| t != NO_TIME);
    match rel {
        Relation::Same if timed => follows(a, b) && follows(b, a),
        Relation::Same => lies_along(a, b) && lies_along(b, a),
        Relation::Within => follows(a, b),
        Relation::Encloses => follows(b, a),
    }
}

/// Indices of up to `SAMPLES` items out of `n`, spread evenly.
fn spread(n: usize) -> impl Iterator<Item = usize> {
    let k = n.min(SAMPLES);
    (0..k).map(move |i| if k == 1 { 0 } else { i * (n - 1) / (k - 1) })
}

fn enough(hits: usize, taken: usize) -> bool {
    taken >= MIN_SAMPLES && hits as f64 >= HIT_SHARE * taken as f64
}

/// Whether `inner` was where `outer` was at the same moments, over the time both cover.
fn follows(inner: &Points, outer: &Points) -> bool {
    let mut by_time: Vec<(i64, usize)> =
        outer.time.iter().enumerate().filter(|(_, t)| **t != NO_TIME).map(|(i, &t)| (t, i)).collect();
    by_time.sort_unstable();
    let (Some(&(first, _)), Some(&(last, _))) = (by_time.first(), by_time.last()) else { return false };
    let timed: Vec<usize> =
        (0..inner.time.len()).filter(|&i| inner.time[i] != NO_TIME && (first..=last).contains(&inner.time[i])).collect();
    let mut hits = 0;
    let mut taken = 0;
    for k in spread(timed.len()) {
        let i = timed[k];
        let t = inner.time[i];
        taken += 1;
        let j = by_time.partition_point(|&(u, _)| u < t);
        let (tq, q) = by_time[j];
        let (tp, p) = if tq == t { (tq, q) } else { by_time[j - 1] };
        if tq - tp > MAX_GAP_MS {
            continue;
        }
        let f = if tq == tp { 0.0 } else { (t - tp) as f64 / (tq - tp) as f64 };
        let (a, b) = (outer.coords[p], outer.coords[q]);
        let at = (a.0 + f * (b.0 - a.0), a.1 + f * (b.1 - a.1));
        let me = inner.coords[i];
        if haversine_m(me.0, me.1, at.0, at.1) <= NEAR_M {
            hits += 1;
        }
    }
    enough(hits, taken)
}

/// Whether `a`'s points lie on `b`'s line, for tracks without times.
fn lies_along(a: &Points, b: &Points) -> bool {
    let mut hits = 0;
    let mut taken = 0;
    for i in spread(a.coords.len()) {
        taken += 1;
        if distance_to_line(a.coords[i], b.coords) <= NEAR_M {
            hits += 1;
        }
    }
    enough(hits, taken)
}

/// Metres from `p` to the nearest segment of `line`, on a local flat projection.
fn distance_to_line(p: (f64, f64), line: &[(f64, f64)]) -> f64 {
    const M_PER_DEG: f64 = 111_195.0;
    let kx = M_PER_DEG * p.0.to_radians().cos();
    let xy = |q: (f64, f64)| ((q.1 - p.1) * kx, (q.0 - p.0) * M_PER_DEG);
    let mut best = f64::INFINITY;
    for (k, &q) in line.iter().enumerate() {
        let (x1, y1) = xy(q);
        let (x0, y0) = if k > 0 { xy(line[k - 1]) } else { (x1, y1) };
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = dx * dx + dy * dy;
        let f = if len2 > 0.0 { (-(x0 * dx + y0 * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let (x, y) = (x0 + f * dx, y0 + f * dy);
        best = best.min((x * x + y * y).sqrt());
    }
    best
}

/// A track's coordinates and times.
type Loaded = (Vec<(f64, f64)>, Vec<i64>);

/// Recomputes the matches of tracks whose `match_version` is outdated: new, re-read or
/// edited ones, or all of them after the rules changed. Returns how many were checked.
pub fn refresh(conn: &mut Connection) -> Result<usize> {
    // Read inside the write lock, so matches never describe geometry an edit just replaced.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let metas: Vec<Meta> = {
        let mut st = tx.prepare("SELECT id, file_id, start_time, end_time, min_lat, min_lon, max_lat, max_lon FROM tracks")?;
        st.query_map([], |r| {
            let bbox = match (r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?) {
                (Some(a), Some(b), Some(c), Some(d)) => Some([a, b, c, d]),
                _ => None,
            };
            Ok(Meta { id: r.get(0)?, file_id: r.get(1)?, start: r.get(2)?, end: r.get(3)?, bbox })
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let stale: Vec<i64> = {
        let mut st = tx.prepare("SELECT id FROM tracks WHERE match_version < ?1")?;
        st.query_map([MATCH_VERSION], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    if stale.is_empty() {
        return Ok(0);
    }
    for id in &stale {
        tx.execute("DELETE FROM track_matches WHERE track_id = ?1 OR other_id = ?1", [id])?;
    }
    let by_id: HashMap<i64, &Meta> = metas.iter().map(|m| (m.id, m)).collect();
    let mut points: HashMap<i64, Loaded> = HashMap::new();
    let mut found = Vec::new();
    let mut done = HashSet::new();
    for &a_id in &stale {
        done.insert(a_id);
        let Some(a) = by_id.get(&a_id) else { continue };
        for b in &metas {
            // A pair of two outdated tracks is checked once, from whichever comes first.
            if done.contains(&b.id) {
                continue;
            }
            let Some(rel) = candidate(a, b) else { continue };
            for id in [a.id, b.id] {
                if let Entry::Vacant(e) = points.entry(id) {
                    let (c, t): (Vec<u8>, Vec<u8>) = tx.query_row(
                        "SELECT coords, time FROM track_data WHERE track_id = ?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    e.insert((codec::decode_coords(&c), codec::decode_i64(&t)));
                }
            }
            let (pa, pb) = (&points[&a.id], &points[&b.id]);
            let (pa, pb) = (Points { coords: &pa.0, time: &pa.1 }, Points { coords: &pb.0, time: &pb.1 });
            if confirm(rel, &pa, &pb) {
                found.push(match rel {
                    Relation::Same => (a.id.min(b.id), a.id.max(b.id), "same"),
                    Relation::Within => (a.id, b.id, "within"),
                    Relation::Encloses => (b.id, a.id, "within"),
                });
            }
        }
    }
    for (a, b, kind) in &found {
        tx.execute(
            "INSERT OR REPLACE INTO track_matches(track_id, other_id, kind) VALUES (?1, ?2, ?3)",
            params![a, b, kind],
        )?;
    }
    for id in &stale {
        tx.execute("UPDATE tracks SET match_version = ?2 WHERE id = ?1", params![id, MATCH_VERSION])?;
    }
    tx.commit()?;
    Ok(stale.len())
}

/// The catalog's matches, with bundles formed from the same-track ones.
#[derive(Default)]
pub struct Bundles {
    /// Bundled track → the bundle's main track, the one shown in the list and on the map.
    pub main: HashMap<i64, i64>,
    /// Track → the tracks that are the same recording, bundled or shown apart.
    pub same: HashMap<i64, Vec<i64>>,
    /// Track → the longer tracks it lies within.
    pub within: HashMap<i64, Vec<i64>>,
    /// Tracks the user keeps out of their bundle.
    pub apart: HashSet<i64>,
}

impl Bundles {
    pub fn members(&self, main: i64) -> Vec<i64> {
        let mut m: Vec<i64> = self.main.iter().filter(|(_, v)| **v == main).map(|(k, _)| *k).collect();
        m.sort_unstable();
        m
    }
}

struct Pick {
    chosen: bool,
    present: bool,
    edited: bool,
    annotated: bool,
    points: i64,
    file_tracks: i64,
    gpx: bool,
}

pub fn bundles(conn: &Connection) -> Result<Bundles> {
    let mut b = Bundles::default();
    let mut pairs = Vec::new();
    {
        let mut st = conn.prepare("SELECT track_id, other_id, kind FROM track_matches")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let (x, y, kind): (i64, i64, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
            if kind == "same" {
                b.same.entry(x).or_default().push(y);
                b.same.entry(y).or_default().push(x);
                pairs.push((x, y));
            } else {
                b.within.entry(x).or_default().push(y);
            }
        }
    }
    if pairs.is_empty() {
        return Ok(b);
    }
    let mut pick: HashMap<i64, Pick> = HashMap::new();
    {
        let mut st = conn.prepare(
            "SELECT t.id, t.bundle_apart, t.bundle_main, f.missing, t.edited,
                    t.title IS NOT NULL OR t.description IS NOT NULL OR t.activity IS NOT NULL
                      OR EXISTS (SELECT 1 FROM track_tags g WHERE g.track_id = t.id),
                    t.point_count, f.format, (SELECT COUNT(*) FROM tracks o WHERE o.file_id = t.file_id)
             FROM tracks t JOIN files f ON f.id = t.file_id
             WHERE t.id IN (SELECT track_id FROM track_matches WHERE kind = 'same'
                            UNION SELECT other_id FROM track_matches WHERE kind = 'same')",
        )?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            if r.get::<_, i64>(1)? != 0 {
                b.apart.insert(id);
            }
            pick.insert(
                id,
                Pick {
                    chosen: r.get::<_, i64>(2)? != 0,
                    present: r.get::<_, i64>(3)? == 0,
                    edited: r.get::<_, i64>(4)? != 0,
                    annotated: r.get::<_, i64>(5)? != 0,
                    points: r.get(6)?,
                    gpx: r.get::<_, String>(7)? == "gpx",
                    file_tracks: r.get(8)?,
                },
            );
        }
    }
    let mut parent: HashMap<i64, i64> = HashMap::new();
    fn root(parent: &mut HashMap<i64, i64>, x: i64) -> i64 {
        let p = *parent.get(&x).unwrap_or(&x);
        if p == x {
            return x;
        }
        let r = root(parent, p);
        parent.insert(x, r);
        r
    }
    for &(x, y) in &pairs {
        if b.apart.contains(&x) || b.apart.contains(&y) {
            continue;
        }
        let (rx, ry) = (root(&mut parent, x), root(&mut parent, y));
        if rx != ry {
            parent.insert(rx.max(ry), rx.min(ry));
        }
    }
    let mut groups: HashMap<i64, Vec<i64>> = HashMap::new();
    for &(x, y) in &pairs {
        for id in [x, y] {
            if !b.apart.contains(&id) {
                let r = root(&mut parent, id);
                let g = groups.entry(r).or_default();
                if !g.contains(&id) {
                    g.push(id);
                }
            }
        }
    }
    // The user's choice, then a track whose file is present, then the user's own work
    // (edits, then title, tags or activity), then the fuller recording, then a file of its
    // own over a collection of many trips, then GPX.
    let key = |id: &i64| {
        let p = &pick[id];
        (p.chosen, p.present, p.edited, p.annotated, p.points, -p.file_tracks, p.gpx, -id)
    };
    for members in groups.values().filter(|g| g.len() > 1) {
        let main = *members.iter().max_by_key(|id| key(id)).unwrap();
        for &id in members {
            b.main.insert(id, main);
        }
    }
    Ok(b)
}

/// Makes `id` the track its bundle shows.
pub fn set_main(conn: &mut Connection, id: i64) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let b = bundles(&tx)?;
    let Some(&main) = b.main.get(&id) else { bail!("This track is not bundled with another.") };
    for m in b.members(main) {
        tx.execute("UPDATE tracks SET bundle_main = ?2 WHERE id = ?1", params![m, (m == id) as i64])?;
    }
    tx.commit()?;
    Ok(())
}

/// Shows `id` on its own instead of in its bundle, or bundles it again.
pub fn set_apart(conn: &Connection, id: i64, apart: bool) -> Result<()> {
    conn.execute("UPDATE tracks SET bundle_apart = ?2, bundle_main = 0 WHERE id = ?1", params![id, apart as i64])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;
    use crate::parse::Format;
    use crate::process::process_file;

    const MIN: i64 = 60_000;

    fn meta(id: i64, file_id: i64, start: i64, end: i64) -> Meta {
        Meta { id, file_id, start: Some(start), end: Some(end), bbox: Some([50.0, 8.0, 50.1, 8.1]) }
    }

    /// A walk north from 50°N 8°E, one fix every `step` ms from `start`, `n` fixes.
    fn walk(start: i64, step: i64, n: usize) -> (Vec<(f64, f64)>, Vec<i64>) {
        // About 1.4 m/s.
        let deg_per_ms = 1.4 / 111_195.0 / 1000.0;
        (0..n)
            .map(|i| {
                let t = start + i as i64 * step;
                ((50.0 + t as f64 * deg_per_ms, 8.0), t)
            })
            .unzip()
    }

    fn pts<'a>(w: &'a (Vec<(f64, f64)>, Vec<i64>)) -> Points<'a> {
        Points { coords: &w.0, time: &w.1 }
    }

    #[test]
    fn spans_decide_the_candidate_relation() {
        assert_eq!(candidate(&meta(1, 1, 0, 100 * MIN), &meta(2, 2, 2 * MIN, 99 * MIN)), Some(Relation::Same));
        assert_eq!(candidate(&meta(1, 1, 10 * MIN, 20 * MIN), &meta(2, 2, 0, 600 * MIN)), Some(Relation::Within));
        assert_eq!(candidate(&meta(1, 1, 0, 600 * MIN), &meta(2, 2, 10 * MIN, 20 * MIN)), Some(Relation::Encloses));
        // Half overlapping, or one after the other.
        assert_eq!(candidate(&meta(1, 1, 0, 100 * MIN), &meta(2, 2, 50 * MIN, 150 * MIN)), None);
        assert_eq!(candidate(&meta(1, 1, 0, 100 * MIN), &meta(2, 2, 100 * MIN, 200 * MIN)), None);
        // Tracks of one file are never duplicates of each other.
        assert_eq!(candidate(&meta(1, 1, 0, 100 * MIN), &meta(2, 1, 0, 100 * MIN)), None);
    }

    #[test]
    fn an_export_with_fewer_fixes_is_the_same_track() {
        let raw = walk(0, 1000, 3600);
        let thinned = walk(0, 5000, 720);
        assert!(confirm(Relation::Same, &pts(&raw), &pts(&thinned)));
    }

    #[test]
    fn same_time_elsewhere_is_not_the_same_track() {
        let a = walk(0, 1000, 3600);
        let mut b = walk(0, 1000, 3600);
        // Another logger the same morning, 1 km east.
        b.0.iter_mut().for_each(|c| c.1 += 0.014);
        assert!(!confirm(Relation::Same, &pts(&a), &pts(&b)));
    }

    #[test]
    fn a_day_within_a_long_dump_is_contained_but_not_the_same() {
        let dump = walk(0, 5000, 20_000);
        let day = walk(30_000_000, 5000, 2_000);
        assert!(confirm(Relation::Within, &pts(&day), &pts(&dump)));
        assert!(confirm(Relation::Encloses, &pts(&dump), &pts(&day)));
        // The dump is not the same track as the day: most of it lies outside.
        let dm = Meta { id: 1, file_id: 1, start: Some(0), end: Some(dump.1[dump.1.len() - 1]), bbox: None };
        let dd = Meta { id: 2, file_id: 2, start: Some(day.1[0]), end: Some(day.1[day.1.len() - 1]), bbox: None };
        assert_eq!(candidate(&dm, &dd), Some(Relation::Encloses));
    }

    #[test]
    fn a_gap_in_the_dump_does_not_contain_the_day() {
        let mut dump = walk(0, 5000, 20_000);
        // The logger of the dump was off during the day's recording.
        dump.0.drain(6_000..6_400);
        dump.1.drain(6_000..6_400);
        let day = walk(30_000_000, 5000, 400);
        assert!(!confirm(Relation::Within, &pts(&day), &pts(&dump)));
    }

    #[test]
    fn untimed_routes_match_by_shape() {
        let (line, _) = walk(0, 1000, 500);
        let none = vec![NO_TIME; 500];
        let sparse: Vec<(f64, f64)> = line.iter().step_by(25).copied().collect();
        let none_sparse = vec![NO_TIME; sparse.len()];
        let a = Points { coords: &line, time: &none };
        let b = Points { coords: &sparse, time: &none_sparse };
        assert!(confirm(Relation::Same, &a, &b));
        // A parallel road 140 m east.
        let detour: Vec<(f64, f64)> = line.iter().map(|c| (c.0, c.1 + 0.002)).collect();
        let c = Points { coords: &detour, time: &none };
        assert!(!confirm(Relation::Same, &a, &c));
    }

    #[test]
    fn timed_and_untimed_tracks_never_match() {
        let timed = meta(1, 1, 0, 100 * MIN);
        let untimed = Meta { start: None, end: None, ..meta(2, 2, 0, 0) };
        assert_eq!(candidate(&timed, &untimed), None);
        assert_eq!(candidate(&Meta { id: 3, file_id: 3, ..untimed.clone() }, &untimed), Some(Relation::Same));
        let elsewhere = Meta { bbox: Some([51.0, 8.0, 51.1, 8.1]), ..untimed.clone() };
        assert_eq!(candidate(&Meta { id: 3, file_id: 3, ..untimed }, &elsewhere), None);
    }

    /// GPX of a walk north from 50°N 8°E, one fix per `step` s from 10:00, `n` fixes.
    fn gpx(step: u32, n: u32) -> String {
        let pts: String = (0..n)
            .map(|i| {
                let s = i * step;
                let lat = 50.0 + s as f64 * 1.4 / 111_195.0;
                format!(
                    r#"<trkpt lat="{lat:.7}" lon="8.0"><time>2024-01-01T{:02}:{:02}:{:02}Z</time></trkpt>"#,
                    10 + s / 3600,
                    s / 60 % 60,
                    s % 60
                )
            })
            .collect();
        format!("<gpx><trk><trkseg>{pts}</trkseg></trk></gpx>")
    }

    struct Cat {
        dir: std::path::PathBuf,
        conn: Connection,
        root: i64,
    }

    impl Cat {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("gtm-dedup-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let conn = catalog::open(&dir.join("c.sqlite")).unwrap();
            let root = catalog::add_root(&conn, dir.to_str().unwrap()).unwrap().id;
            Cat { dir, conn, root }
        }

        fn import(&mut self, name: &str, content: &str) -> i64 {
            let f = self.dir.join(name);
            std::fs::write(&f, content).unwrap();
            catalog::write_import(&mut self.conn, self.root, &process_file(&f, Format::Gpx)).unwrap();
            let path = f.to_str().unwrap().to_string();
            catalog::list_tracks(&self.conn).unwrap().into_iter().find(|t| t.file_path == path).unwrap().id
        }

        fn summary(&self, id: i64) -> catalog::TrackSummary {
            catalog::list_tracks(&self.conn).unwrap().into_iter().find(|t| t.id == id).unwrap()
        }
    }

    impl Drop for Cat {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    #[test]
    fn copies_are_bundled_and_the_user_choice_survives_a_rescan() {
        let mut c = Cat::new("bundle");
        let raw = c.import("raw.gpx", &gpx(5, 720));
        let export = c.import("export.gpx", &gpx(10, 360));
        // A two-hour dump the first hour lies within.
        let dump = c.import("dump.gpx", &gpx(5, 1440));
        assert_eq!(refresh(&mut c.conn).unwrap(), 3);
        assert_eq!(refresh(&mut c.conn).unwrap(), 0);

        // The fuller recording is shown.
        assert_eq!(c.summary(export).bundle_main, Some(raw));
        assert_eq!(c.summary(raw).same_as, vec![export]);
        assert_eq!(c.summary(dump).bundle_main, None);
        let mut within = c.summary(raw).within;
        within.sort();
        assert_eq!(within, vec![dump]);

        set_main(&mut c.conn, export).unwrap();
        assert_eq!(c.summary(raw).bundle_main, Some(export));
        // Rescanning an unchanged file keeps the track, its choice and its matches.
        c.import("export.gpx", &gpx(10, 360));
        refresh(&mut c.conn).unwrap();
        assert_eq!(c.summary(raw).bundle_main, Some(export));

        set_apart(&c.conn, raw, true).unwrap();
        let r = c.summary(raw);
        assert_eq!((r.bundle_main, r.bundle_apart, r.same_as), (None, true, vec![export]));
        assert_eq!(c.summary(export).bundle_main, None);
        set_apart(&c.conn, raw, false).unwrap();
        assert!(c.summary(raw).bundle_main.is_some());
    }

    #[test]
    fn edits_and_removed_tracks_update_the_matches() {
        let mut c = Cat::new("edit");
        let a = c.import("a.gpx", &gpx(5, 720));
        let b = c.import("b.gpx", &gpx(5, 720));
        refresh(&mut c.conn).unwrap();
        assert_eq!(c.summary(a).bundle_main, Some(a.min(b)));

        // Cutting the track down to its first quarter leaves it within the other.
        let rest: Vec<u32> = (180..720).collect();
        crate::edit::delete_points(&mut c.conn, a, &rest).unwrap();
        assert_eq!(refresh(&mut c.conn).unwrap(), 1);
        let s = c.summary(a);
        assert_eq!((s.bundle_main, s.within), (None, vec![b]));

        // A different walk: no match left.
        c.import("b.gpx", &gpx(5, 720).replace(r#"lon="8.0""#, r#"lon="8.1""#));
        refresh(&mut c.conn).unwrap();
        assert!(c.summary(a).within.is_empty());
    }
}
