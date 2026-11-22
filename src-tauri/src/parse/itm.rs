use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::model::{ParsedFile, ParsedTrack, Points, Waypoint};

/// `fObjType` of POIs in `PropInt` (the `fUserType` of `POI` in `IT_TGVLib`).
const POI_OBJ_TYPE: i64 = 9;

struct Wp {
    id: i64,
    next: i64,
    lat: f64,
    lon: f64,
    ele: Option<f32>,
    time: Option<i64>,
}

/// Holux ezTour project: `ituser.poi` from the `.itm` ZIP, an SQLite database. Every `Line`
/// is a track whose points (`WP`, linked by `fNextID`) carry their time in `GPSLog`; `POI`
/// rows are waypoints. Points ezTour deleted stay in the table with parent 0.
pub fn parse(db: &[u8]) -> Result<ParsedFile> {
    // In memory: a temporary file would put the location history where others may read it.
    let mut conn = Connection::open_in_memory()?;
    conn.deserialize_read_exact("main", db, db.len(), true).context("not a Holux ezTour project")?;

    let mut by_line: HashMap<i64, Vec<Wp>> = HashMap::new();
    let mut st = conn
        .prepare(
            "SELECT w.fParentID, w.fID, w.fNextID, w.fLatitude, w.fLongitude, w.fAltitude, g.fGPSTime1, g.fGPSTime2
             FROM WP w LEFT JOIN GPSLog g ON g.fID = w.fGPSLogID AND w.fGPSLogID != 0
             WHERE w.fParentID != 0 ORDER BY w.fID",
        )
        .context("not a Holux ezTour project")?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        by_line.entry(r.get(0)?).or_default().push(Wp {
            id: r.get(1)?,
            next: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            lat: r.get::<_, Option<f64>>(3)?.unwrap_or(f64::NAN),
            lon: r.get::<_, Option<f64>>(4)?.unwrap_or(f64::NAN),
            ele: r.get::<_, Option<f64>>(5)?.map(|e| e as f32),
            time: filetime(r.get(6)?, r.get(7)?),
        });
    }

    let mut tracks = Vec::new();
    let mut st = conn.prepare("SELECT fID, fName, f1stWPID FROM Line WHERE fIsGroup = 0 ORDER BY fID")?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let Some(wps) = by_line.get(&r.get::<_, i64>(0)?) else { continue };
        let name: Option<String> = r.get(1)?;
        let mut points = Points::default();
        for w in chain(wps, r.get::<_, Option<i64>>(2)?.unwrap_or(0)) {
            if w.lat.is_finite() && w.lon.is_finite() && w.lat.abs() <= 90.0 && w.lon.abs() <= 180.0 {
                points.push(w.lat, w.lon, w.ele, w.time);
            }
        }
        if !points.is_empty() {
            let name = name.filter(|n| !n.trim().is_empty());
            tracks.push(ParsedTrack { name, description: None, activity: None, points });
        }
    }

    let mut waypoints = Vec::new();
    let mut st = conn.prepare(&format!(
        "SELECT p.fName, p.fRealLati, p.fRealLong, p.fLatitude, p.fLongitude, p.fAltitude, t1.fValue, t2.fValue
         FROM POI p
         LEFT JOIN PropInt t1 ON t1.fObjType = {POI_OBJ_TYPE} AND t1.fObjID = p.fID AND t1.fName = 't1'
         LEFT JOIN PropInt t2 ON t2.fObjType = {POI_OBJ_TYPE} AND t2.fObjID = p.fID AND t2.fName = 't2'
         ORDER BY p.fID"
    ))?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let valid = |lat: Option<f64>, lon: Option<f64>| {
            lat.zip(lon).filter(|&(a, o)| a.abs() <= 90.0 && o.abs() <= 180.0 && (a, o) != (-1.0, -1.0) && (a, o) != (0.0, 0.0))
        };
        // fReal* is the position, 0/0 when unset; fLatitude/fLongitude are -1 on the global map.
        let Some((lat, lon)) = valid(r.get(1)?, r.get(2)?).or(valid(r.get(3)?, r.get(4)?)) else { continue };
        let name: Option<String> = r.get(0)?;
        waypoints.push(Waypoint {
            kind: Waypoint::kind_for_name(name.as_deref()),
            name,
            description: None,
            symbol: None,
            lat,
            lon,
            ele: r.get::<_, Option<f64>>(5)?.map(|e| e as f32),
            time: filetime(r.get(6)?, r.get(7)?),
        });
    }
    Ok(ParsedFile { tracks, waypoints })
}

/// Points in `fNextID` order from `first`; points the chain misses follow in id order.
fn chain(wps: &[Wp], first: i64) -> Vec<&Wp> {
    let index: HashMap<i64, usize> = wps.iter().enumerate().map(|(i, w)| (w.id, i)).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(wps.len());
    let mut at = index.get(&first).copied().or((!wps.is_empty()).then_some(0));
    while let Some(i) = at {
        if !seen.insert(i) {
            break;
        }
        out.push(&wps[i]);
        at = index.get(&wps[i].next).copied();
    }
    if out.len() < wps.len() {
        out.extend(wps.iter().enumerate().filter(|(i, _)| !seen.contains(i)).map(|(_, w)| w));
    }
    out
}

/// A Windows FILETIME (100 ns since 1601, UTC) split into its high and low 32 bits, the low
/// half stored signed.
fn filetime(hi: Option<i64>, lo: Option<i64>) -> Option<i64> {
    let (hi, lo) = (hi?, lo?);
    if hi <= 0 {
        return None;
    }
    let ticks = ((hi as u64) << 32) | (lo as u32 as u64);
    let ms = (ticks / 10_000) as i64 - 11_644_473_600_000;
    (ms > 0).then_some(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_db() -> Vec<u8> {
        let path = std::env::temp_dir().join(format!("gpstrackmanager-itm-test-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE POI (fID INT PRIMARY KEY, fZero INT, fName VARCHAR(128), fTypeID INT, fMapID INT,
               fLongitude DOUBLE, fLatitude DOUBLE, fRealLong DOUBLE, fRealLati DOUBLE, fAltitude DOUBLE);
             CREATE TABLE WP (fID INT PRIMARY KEY, fZero INT, fParentID INT, fNextID INT, fGPSLogID INT, fMapID INT,
               fPosX INT, fPosY INT, fLongitude DOUBLE, fLatitude DOUBLE, fAltitude DOUBLE, fOrder INT);
             CREATE TABLE Line (fID INT PRIMARY KEY, fZero INT, fName VARCHAR(256), fTypeID INT, fIsGroup INT,
               fParentID INT, fVPID INT, fStartID INT, fEndID INT, f1stWPID INT, fLstWPID INT, fMapID INT);
             CREATE TABLE PropInt (fID INT PRIMARY KEY, fZero INT, fName VARCHAR(256), fObjType INT, fObjID INT,
               fUserFlag INT, fValue INT);
             CREATE TABLE GPSLog (fID INT PRIMARY KEY, fZero INT, fGPSTime1 INT, fGPSTime2 INT, fGPSSpeed DOUBLE);
             INSERT INTO Line VALUES (1,0,'2012-10-06_Thun',2,0,0,0,0,0,1,4,1), (2,0,'Group',2,1,0,0,0,0,0,0,1);
             -- 3 was deleted in ezTour (parent 0); 4 comes before 2 in the chain.
             INSERT INTO WP VALUES (1,0,1,4,1,1,-1,-1,7.626297,46.744408,564.4,0), (2,0,1,0,2,1,-1,-1,7.63,46.75,565,0),
               (3,0,0,0,3,1,-1,-1,1,1,1,0), (4,0,1,2,0,1,-1,-1,7.627,46.745,564.5,0);
             INSERT INTO GPSLog VALUES (1,0,30254041,1103156864,0), (2,0,30254041,1123156864,0), (3,0,0,0,0);
             INSERT INTO POI VALUES (1,0,'Lat:46.397266',1,0,-1,-1,7.332787,46.397266,1380.23),
               (2,0,'Unset',1,0,-1,-1,0,0,0);
             INSERT INTO PropInt VALUES (1,0,'t1',9,1,0,30302270), (2,0,'t2',9,1,0,165438080),
               (3,0,'t1',11,1,0,1), (4,0,'t2',11,1,0,1);",
        )
        .unwrap();
        drop(conn);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        bytes
    }

    #[test]
    fn reads_lines_in_chain_order_and_pois() {
        let f = parse(&sample_db()).unwrap();
        assert_eq!(f.tracks.len(), 1);
        let t = &f.tracks[0];
        assert_eq!(t.name.as_deref(), Some("2012-10-06_Thun"));
        assert_eq!(t.points.lat, vec![46.744408, 46.745, 46.75]);
        // 2012-10-06T15:42:57Z, as ezTour's own GPX export of the same project says.
        assert_eq!(t.points.time[0], 1349538177000);
        assert_eq!(t.points.time[1], crate::model::NO_TIME);
        assert_eq!(t.points.time[2], 1349538179000);
        assert_eq!(f.waypoints.len(), 1, "a POI without a position is skipped");
        let w = &f.waypoints[0];
        assert_eq!((w.lat, w.lon, w.ele), (46.397266, 7.332787, Some(1380.23)));
        assert_eq!(w.time, Some(1370252281000));
    }

    #[test]
    fn rejects_other_databases() {
        assert!(parse(b"not sqlite").is_err());
    }
}
