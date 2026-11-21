use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::codec;
use crate::process::FileImport;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS roots(
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS files(
  id INTEGER PRIMARY KEY,
  root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
  path TEXT NOT NULL UNIQUE,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  hash TEXT NOT NULL,
  format TEXT NOT NULL,
  status TEXT NOT NULL,          -- ok | error | unsupported
  error TEXT,
  missing INTEGER NOT NULL DEFAULT 0,
  scanned_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS files_hash ON files(hash);
CREATE TABLE IF NOT EXISTS tracks(
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL,           -- position in the file; negative for parts split off in the app
  name TEXT,                     -- from the file
  file_description TEXT,         -- from the file
  title TEXT,                    -- user override, survives rescans
  description TEXT,              -- user, survives rescans
  file_activity TEXT,            -- from the file (FIT sport, GPX type)
  activity TEXT,                 -- user override
  start_time INTEGER,
  end_time INTEGER,
  point_count INTEGER NOT NULL,
  distance_m REAL NOT NULL,
  min_lat REAL, min_lon REAL, max_lat REAL, max_lon REAL,
  min_ele REAL, max_ele REAL,
  edited INTEGER NOT NULL DEFAULT 0, -- points edited in the app; rescans leave the track alone
  UNIQUE(file_id, idx)
);
CREATE INDEX IF NOT EXISTS tracks_start ON tracks(start_time);
CREATE TABLE IF NOT EXISTS track_data(
  track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
  coords BLOB NOT NULL,
  ele BLOB NOT NULL,
  time BLOB NOT NULL,
  importance BLOB NOT NULL,
  segs BLOB NOT NULL,
  overview BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS tags(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE IF NOT EXISTS track_tags(
  track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  PRIMARY KEY(track_id, tag_id)
);
"#;

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        let has_edited: bool =
            conn.query_row("SELECT COUNT(*) FROM pragma_table_info('tracks') WHERE name = 'edited'", [], |r| r.get(0))?;
        if !has_edited {
            conn.execute_batch("ALTER TABLE tracks ADD COLUMN edited INTEGER NOT NULL DEFAULT 0")?;
        }
        conn.pragma_update(None, "user_version", 1)?;
    }
    if version < 2 {
        for col in ["file_activity", "activity"] {
            let exists: bool = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tracks') WHERE name = ?1",
                [col],
                |r| r.get(0),
            )?;
            if !exists {
                conn.execute_batch(&format!("ALTER TABLE tracks ADD COLUMN {col} TEXT"))?;
            }
        }
        conn.pragma_update(None, "user_version", 2)?;
    }
    Ok(())
}

#[derive(Serialize, Clone)]
pub struct Root {
    pub id: i64,
    pub path: String,
}

pub fn list_roots(conn: &Connection) -> Result<Vec<Root>> {
    let mut st = conn.prepare("SELECT id, path FROM roots ORDER BY path")?;
    let rows = st.query_map([], |r| Ok(Root { id: r.get(0)?, path: r.get(1)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn add_root(conn: &Connection, path: &str) -> Result<Root> {
    conn.execute("INSERT OR IGNORE INTO roots(path) VALUES (?1)", [path])?;
    let id = conn.query_row("SELECT id FROM roots WHERE path = ?1", [path], |r| r.get(0))?;
    Ok(Root { id, path: path.to_string() })
}

pub fn get_root(conn: &Connection, id: i64) -> Result<Root> {
    Ok(conn.query_row("SELECT id, path FROM roots WHERE id = ?1", [id], |r| {
        Ok(Root { id: r.get(0)?, path: r.get(1)? })
    })?)
}

/// Removes the root from the catalog only; files on disk are untouched.
pub fn remove_root(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM roots WHERE id = ?1", [id])?;
    Ok(())
}

pub struct KnownFile {
    pub size: i64,
    pub mtime: i64,
}

pub fn known_files(conn: &Connection, root_id: i64) -> Result<HashMap<String, KnownFile>> {
    let mut st = conn.prepare("SELECT path, size, mtime FROM files WHERE root_id = ?1")?;
    let rows = st.query_map([root_id], |r| Ok((r.get::<_, String>(0)?, KnownFile { size: r.get(1)?, mtime: r.get(2)? })))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn hash_known(conn: &Connection, hash: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT 1 FROM files WHERE hash = ?1 LIMIT 1", [hash], |_| Ok(())).optional()?.is_some())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn record_unsupported(conn: &Connection, root_id: i64, path: &str, size: i64, mtime: i64) -> Result<()> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    conn.execute(
        "INSERT INTO files(root_id, path, size, mtime, hash, format, status, scanned_at)
         VALUES (?1, ?2, ?3, ?4, '', ?5, 'unsupported', ?6)
         ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, missing = 0",
        params![root_id, path, size, mtime, ext, now_ms()],
    )?;
    Ok(())
}

/// Writes one imported file. Tracks are matched by their index inside the file so user
/// titles, descriptions and tags survive when a changed file is re-imported.
pub fn write_import(conn: &mut Connection, root_id: i64, imp: &FileImport) -> Result<()> {
    let tx = conn.transaction()?;
    let (status, error) = match &imp.result {
        Ok(_) => ("ok", None),
        Err(e) => ("error", Some(e.as_str())),
    };
    let file_id: i64 = tx.query_row(
        "INSERT INTO files(root_id, path, size, mtime, hash, format, status, error, missing, scanned_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)
         ON CONFLICT(path) DO UPDATE SET root_id = excluded.root_id, size = excluded.size,
           mtime = excluded.mtime, hash = excluded.hash, format = excluded.format,
           status = excluded.status, error = excluded.error, missing = 0, scanned_at = excluded.scanned_at
         RETURNING id",
        params![root_id, imp.path, imp.size, imp.mtime, imp.hash, imp.format, status, error, now_ms()],
        |r| r.get(0),
    )?;

    let tracks = imp.result.as_deref().unwrap_or(&[]);
    for (idx, t) in tracks.iter().enumerate() {
        let s = &t.stats;
        let bbox = s.bbox.map(|b| b.map(Some)).unwrap_or([None; 4]);
        let track_id: Option<i64> = tx.query_row(
            "INSERT INTO tracks(file_id, idx, name, file_description, start_time, end_time, point_count,
               distance_m, min_lat, min_lon, max_lat, max_lon, min_ele, max_ele, file_activity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(file_id, idx) DO UPDATE SET name = excluded.name,
               file_description = excluded.file_description, file_activity = excluded.file_activity,
               start_time = excluded.start_time,
               end_time = excluded.end_time, point_count = excluded.point_count,
               distance_m = excluded.distance_m, min_lat = excluded.min_lat, min_lon = excluded.min_lon,
               max_lat = excluded.max_lat, max_lon = excluded.max_lon, min_ele = excluded.min_ele,
               max_ele = excluded.max_ele
             WHERE tracks.edited = 0
             RETURNING id",
            params![
                file_id, idx as i64, t.name, t.description, s.start_time, s.end_time,
                s.point_count as i64, s.distance_m, bbox[0], bbox[1], bbox[2], bbox[3], s.min_ele, s.max_ele,
                t.activity
            ],
            |r| r.get(0),
        ).optional()?;
        let Some(track_id) = track_id else { continue };
        let b = &t.blobs;
        tx.execute(
            "INSERT OR REPLACE INTO track_data(track_id, coords, ele, time, importance, segs, overview)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![track_id, b.coords, b.ele, b.time, b.importance, b.segs, b.overview],
        )?;
    }
    tx.execute("DELETE FROM tracks WHERE file_id = ?1 AND idx >= ?2", params![file_id, tracks.len() as i64])?;
    tx.commit()?;
    Ok(())
}

/// Flags catalogued files that were not found on disk, like Lightroom's "missing" state.
pub fn update_missing(conn: &mut Connection, root_id: i64, seen: &[String]) -> Result<usize> {
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS seen(path TEXT PRIMARY KEY); DELETE FROM seen;")?;
    {
        let mut ins = tx.prepare("INSERT OR IGNORE INTO seen(path) VALUES (?1)")?;
        for p in seen {
            ins.execute([p])?;
        }
    }
    tx.execute("UPDATE files SET missing = 0 WHERE root_id = ?1 AND path IN (SELECT path FROM seen)", [root_id])?;
    let missing =
        tx.execute("UPDATE files SET missing = 1 WHERE root_id = ?1 AND path NOT IN (SELECT path FROM seen)", [root_id])?;
    tx.commit()?;
    Ok(missing)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackSummary {
    pub id: i64,
    pub file_id: i64,
    pub file_path: String,
    pub format: String,
    pub idx: i64,
    pub track_count_in_file: i64,
    pub name: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub file_description: Option<String>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub point_count: i64,
    pub distance_m: f64,
    pub bbox: Option<[f64; 4]>,
    pub min_ele: Option<f64>,
    pub max_ele: Option<f64>,
    pub tags: Vec<String>,
    pub missing: bool,
    pub edited: bool,
    /// User choice; `None` means "as in the file".
    pub activity: Option<String>,
    pub file_activity: Option<String>,
}

pub fn list_tracks(conn: &Connection) -> Result<Vec<TrackSummary>> {
    let mut st = conn.prepare(
        "SELECT t.id, t.file_id, f.path, f.format, t.idx,
                (SELECT COUNT(*) FROM tracks t2 WHERE t2.file_id = t.file_id),
                t.name, t.title, t.description, t.file_description, t.start_time, t.end_time,
                t.point_count, t.distance_m, t.min_lat, t.min_lon, t.max_lat, t.max_lon,
                t.min_ele, t.max_ele,
                (SELECT group_concat(g.name, char(31)) FROM track_tags tt JOIN tags g ON g.id = tt.tag_id
                  WHERE tt.track_id = t.id),
                f.missing, t.edited, t.activity, t.file_activity
         FROM tracks t JOIN files f ON f.id = t.file_id
         ORDER BY t.start_time DESC NULLS LAST, f.path, t.idx",
    )?;
    let rows = st.query_map([], |r| {
        let bbox = match (r.get::<_, Option<f64>>(14)?, r.get(15)?, r.get(16)?, r.get(17)?) {
            (Some(a), Some(b), Some(c), Some(d)) => Some([a, b, c, d]),
            _ => None,
        };
        let tags: Option<String> = r.get(20)?;
        Ok(TrackSummary {
            id: r.get(0)?,
            file_id: r.get(1)?,
            file_path: r.get(2)?,
            format: r.get(3)?,
            idx: r.get(4)?,
            track_count_in_file: r.get(5)?,
            name: r.get(6)?,
            title: r.get(7)?,
            description: r.get(8)?,
            file_description: r.get(9)?,
            start_time: r.get(10)?,
            end_time: r.get(11)?,
            point_count: r.get(12)?,
            distance_m: r.get(13)?,
            bbox,
            min_ele: r.get(18)?,
            max_ele: r.get(19)?,
            tags: tags.map(|s| s.split('\u{1f}').map(String::from).collect()).unwrap_or_default(),
            missing: r.get::<_, i64>(21)? != 0,
            edited: r.get::<_, i64>(22)? != 0,
            activity: r.get(23)?,
            file_activity: r.get(24)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileProblem {
    pub path: String,
    pub status: String,
    pub error: Option<String>,
    pub missing: bool,
}

pub fn list_problem_files(conn: &Connection) -> Result<Vec<FileProblem>> {
    let mut st = conn.prepare(
        "SELECT path, status, error, missing FROM files WHERE status != 'ok' OR missing = 1 ORDER BY path",
    )?;
    let rows = st.query_map([], |r| {
        Ok(FileProblem { path: r.get(0)?, status: r.get(1)?, error: r.get(2)?, missing: r.get::<_, i64>(3)? != 0 })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub struct TrackData {
    pub coords: Vec<(f64, f64)>,
    pub ele: Vec<f32>,
    pub time: Vec<i64>,
    pub importance: Vec<f32>,
    pub seg_starts: Vec<u32>,
}

pub fn load_track_data(conn: &Connection, track_id: i64) -> Result<TrackData> {
    Ok(conn.query_row(
        "SELECT coords, ele, time, importance, segs FROM track_data WHERE track_id = ?1",
        [track_id],
        |r| {
            Ok(TrackData {
                coords: codec::decode_coords(&r.get::<_, Vec<u8>>(0)?),
                ele: codec::decode_f32(&r.get::<_, Vec<u8>>(1)?),
                time: codec::decode_i64(&r.get::<_, Vec<u8>>(2)?),
                importance: codec::decode_f32(&r.get::<_, Vec<u8>>(3)?),
                seg_starts: codec::decode_u32(&r.get::<_, Vec<u8>>(4)?),
            })
        },
    )?)
}

/// Binary overview of all tracks: per track `[id, n, lon0, lat0, …]` as f64,
/// NaN pairs marking segment breaks.
pub fn overview(conn: &Connection) -> Result<Vec<f64>> {
    let mut st = conn.prepare(
        "SELECT d.track_id, d.overview FROM track_data d JOIN tracks t ON t.id = d.track_id
         JOIN files f ON f.id = t.file_id WHERE f.missing = 0",
    )?;
    let mut out = Vec::new();
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let coords = codec::decode_coords(&r.get::<_, Vec<u8>>(1)?);
        out.push(id as f64);
        out.push(coords.len() as f64);
        for (lat, lon) in coords {
            out.push(lon);
            out.push(lat);
        }
    }
    Ok(out)
}

pub fn update_track_meta(conn: &Connection, id: i64, title: Option<&str>, description: Option<&str>) -> Result<()> {
    let clean = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    conn.execute(
        "UPDATE tracks SET title = ?2, description = ?3 WHERE id = ?1",
        params![id, clean(title), clean(description)],
    )?;
    Ok(())
}

pub fn set_track_activity(conn: &Connection, id: i64, activity: Option<&str>) -> Result<()> {
    conn.execute("UPDATE tracks SET activity = ?2 WHERE id = ?1", params![id, activity.filter(|a| !a.is_empty())])?;
    Ok(())
}

pub fn set_track_tags(conn: &mut Connection, id: i64, tags: &[String]) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM track_tags WHERE track_id = ?1", [id])?;
    for tag in tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        tx.execute("INSERT OR IGNORE INTO tags(name) VALUES (?1)", [tag])?;
        tx.execute(
            "INSERT OR IGNORE INTO track_tags(track_id, tag_id) SELECT ?1, id FROM tags WHERE name = ?2",
            params![id, tag],
        )?;
    }
    tx.execute("DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM track_tags)", [])?;
    tx.commit()?;
    Ok(())
}

pub fn list_tags(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT name FROM tags ORDER BY name COLLATE NOCASE")?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
