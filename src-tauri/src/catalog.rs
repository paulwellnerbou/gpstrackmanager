use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;

use crate::codec;
use crate::model::Waypoint;
use crate::places::{self, PLACES_VERSION, Place, Role, TrackPlace};
use crate::process::{FileImport, PARSER_VERSION, ProcessedTrack};

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
  scanned_at INTEGER NOT NULL,
  parser_version INTEGER NOT NULL DEFAULT 0
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
  places_version INTEGER NOT NULL DEFAULT 0, -- places::PLACES_VERSION the places were found with
  match_version INTEGER NOT NULL DEFAULT 0,  -- dedup::MATCH_VERSION the matches were found with
  bundle_main INTEGER NOT NULL DEFAULT 0,    -- user: the track its bundle shows
  bundle_apart INTEGER NOT NULL DEFAULT 0,   -- user: shown on its own, not in its bundle
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
  overview BLOB NOT NULL,
  file_segs BLOB,                -- segment starts without glitch splits; NULL in old catalogs
  speed BLOB,                    -- recorded speed (f32 m/s, NaN unknown); NULL when none recorded
  course BLOB                    -- recorded course (f32 degrees); NULL when none recorded
);
CREATE TABLE IF NOT EXISTS track_matches( -- tracks of different files that are one recording
  track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  other_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,            -- same (track_id < other_id) | within (track_id lies within other_id)
  PRIMARY KEY(track_id, other_id)
);
CREATE INDEX IF NOT EXISTS track_matches_other ON track_matches(other_id);
CREATE TABLE IF NOT EXISTS track_places(
  track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  role TEXT NOT NULL,            -- start | via | end
  name TEXT NOT NULL,
  region TEXT,
  country TEXT,
  PRIMARY KEY(track_id, seq)
);
CREATE TABLE IF NOT EXISTS waypoints(
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL,
  name TEXT,
  description TEXT,
  symbol TEXT,
  kind TEXT NOT NULL,            -- waypoint | lap
  lat REAL NOT NULL,
  lon REAL NOT NULL,
  ele REAL,
  time INTEGER
);
CREATE INDEX IF NOT EXISTS waypoints_file ON waypoints(file_id);
CREATE TABLE IF NOT EXISTS tags(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE IF NOT EXISTS track_tags(
  track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  PRIMARY KEY(track_id, tag_id)
);
CREATE TABLE IF NOT EXISTS written_hashes( -- contents a file had before "Write to file" replaced it
  hash TEXT NOT NULL,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  PRIMARY KEY(hash, file_id)
);
CREATE TABLE IF NOT EXISTS edit_undo(  -- point edits that can be undone, newest last per file
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  created_track_id INTEGER       -- split-off part the edit created; removed on undo
);
CREATE INDEX IF NOT EXISTS edit_undo_file ON edit_undo(file_id);
CREATE TABLE IF NOT EXISTS edit_undo_tracks( -- tracks as they were before the edit
  undo_id INTEGER NOT NULL REFERENCES edit_undo(id) ON DELETE CASCADE,
  track_id INTEGER NOT NULL,
  start_time INTEGER, end_time INTEGER, point_count INTEGER NOT NULL, distance_m REAL NOT NULL,
  min_lat REAL, min_lon REAL, max_lat REAL, max_lon REAL, min_ele REAL, max_ele REAL,
  edited INTEGER NOT NULL,
  coords BLOB NOT NULL, ele BLOB NOT NULL, time BLOB NOT NULL, importance BLOB NOT NULL,
  segs BLOB NOT NULL, overview BLOB NOT NULL, file_segs BLOB, speed BLOB, course BLOB,
  PRIMARY KEY(undo_id, track_id)
);
"#;

/// Runs after `migrate`, which adds the columns these refer to in older catalogs.
const TRIGGERS: &str = r#"
-- Any geometry write, also by builds that do not know about places, invalidates them;
-- write_places runs after the geometry and marks them current again.
CREATE TRIGGER IF NOT EXISTS track_data_insert_places AFTER INSERT ON track_data BEGIN
  UPDATE tracks SET places_version = 0 WHERE id = NEW.track_id;
END;
CREATE TRIGGER IF NOT EXISTS track_data_update_places AFTER UPDATE ON track_data BEGIN
  UPDATE tracks SET places_version = 0 WHERE id = NEW.track_id;
END;
-- Likewise for the matches with other files' tracks; dedup::refresh finds them again.
CREATE TRIGGER IF NOT EXISTS track_data_insert_matches AFTER INSERT ON track_data BEGIN
  UPDATE tracks SET match_version = 0 WHERE id = NEW.track_id;
END;
CREATE TRIGGER IF NOT EXISTS track_data_update_matches AFTER UPDATE ON track_data BEGIN
  UPDATE tracks SET match_version = 0 WHERE id = NEW.track_id;
END;
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
    conn.execute_batch(TRIGGERS)?;
    Ok(conn)
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        [table, column],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    if !has_column(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

/// Brings catalogs created by older versions up to the current schema. Fresh catalogs
/// get every column from `SCHEMA`, so each step only adds what may be missing.
fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        add_column(conn, "tracks", "edited", "INTEGER NOT NULL DEFAULT 0")?;
    }
    if version < 2 {
        add_column(conn, "tracks", "file_activity", "TEXT")?;
        add_column(conn, "tracks", "activity", "TEXT")?;
    }
    if version < 3 {
        add_column(conn, "files", "parser_version", "INTEGER NOT NULL DEFAULT 0")?;
        add_column(conn, "track_data", "file_segs", "BLOB")?;
    }
    if version < 4 {
        add_column(conn, "tracks", "places_version", "INTEGER NOT NULL DEFAULT 0")?;
    }
    if version < 5 {
        add_column(conn, "tracks", "match_version", "INTEGER NOT NULL DEFAULT 0")?;
        add_column(conn, "tracks", "bundle_main", "INTEGER NOT NULL DEFAULT 0")?;
        add_column(conn, "tracks", "bundle_apart", "INTEGER NOT NULL DEFAULT 0")?;
    }
    if version < 6 {
        add_column(conn, "track_data", "speed", "BLOB")?;
        add_column(conn, "track_data", "course", "BLOB")?;
        add_column(conn, "edit_undo_tracks", "speed", "BLOB")?;
        add_column(conn, "edit_undo_tracks", "course", "BLOB")?;
    }
    conn.pragma_update(None, "user_version", 6)?;
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

/// Opens a write transaction that takes the write lock immediately. A deferred one that
/// reads first fails at once (SQLITE_BUSY_SNAPSHOT, no busy wait) if the background scan
/// commits in between.
pub fn write_tx(conn: &mut Connection) -> Result<Transaction<'_>> {
    Ok(conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

/// Library folders must not overlap: a file under two roots would switch owner on every
/// scan, and removing either root would delete its catalog entries.
pub fn add_root(conn: &Connection, path: &str) -> Result<Root> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf());
    let path = canonical.to_string_lossy().into_owned();
    for root in list_roots(conn)? {
        if root.path == path {
            return Ok(root);
        }
        let other = Path::new(&root.path);
        if canonical.starts_with(other) {
            anyhow::bail!("{path} is already part of the library folder {}", root.path);
        }
        if other.starts_with(&canonical) {
            anyhow::bail!("{path} contains the library folder {}; remove that one first", root.path);
        }
    }
    conn.execute("INSERT INTO roots(path) VALUES (?1)", [&path])?;
    Ok(Root { id: conn.last_insert_rowid(), path })
}

pub fn get_root(conn: &Connection, id: i64) -> Result<Root> {
    Ok(conn.query_row("SELECT id, path FROM roots WHERE id = ?1", [id], |r| {
        Ok(Root { id: r.get(0)?, path: r.get(1)? })
    })?)
}

/// Removes the root from the catalog only; files on disk are untouched.
pub fn remove_root(conn: &mut Connection, id: i64) -> Result<()> {
    let tx = write_tx(conn)?;
    tx.execute("DELETE FROM roots WHERE id = ?1", [id])?;
    delete_unused_tags(&tx)?;
    tx.commit()?;
    Ok(())
}

pub fn delete_unused_tags(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM track_tags)", [])?;
    Ok(())
}

pub struct KnownFile {
    pub size: i64,
    pub mtime: i64,
    pub parser_version: i64,
    /// Catalogued before a reader for its format existed.
    pub unsupported: bool,
}

pub fn known_files(conn: &Connection, root_id: i64) -> Result<HashMap<String, KnownFile>> {
    let mut st =
        conn.prepare("SELECT path, size, mtime, parser_version, status = 'unsupported' FROM files WHERE root_id = ?1")?;
    let rows = st.query_map([root_id], |r| {
        let file = KnownFile { size: r.get(1)?, mtime: r.get(2)?, parser_version: r.get(3)?, unsupported: r.get(4)? };
        Ok((r.get::<_, String>(0)?, file))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Also true for contents a file had before it was written: the device still holds them.
pub fn hash_known(conn: &Connection, hash: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM files WHERE hash = ?1 UNION ALL SELECT 1 FROM written_hashes WHERE hash = ?1 LIMIT 1",
            [hash],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub fn all_hashes(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut st = conn.prepare("SELECT hash FROM files WHERE hash IS NOT NULL UNION SELECT hash FROM written_hashes")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Whether "Write to file" replaced the catalogued file at `path`, so it holds the user's edits.
pub fn was_written(conn: &Connection, path: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM written_hashes w JOIN files f ON f.id = w.file_id WHERE f.path = ?1 LIMIT 1",
            [path],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn record_unsupported(conn: &Connection, root_id: i64, path: &str, size: i64, mtime: i64) -> Result<()> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    conn.execute(
        "INSERT INTO files(root_id, path, size, mtime, hash, format, status, scanned_at, parser_version)
         VALUES (?1, ?2, ?3, ?4, '', ?5, 'unsupported', ?6, ?7)
         ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, missing = 0,
           parser_version = excluded.parser_version",
        params![root_id, path, size, mtime, ext, now_ms(), PARSER_VERSION],
    )?;
    Ok(())
}

/// Writes one imported file in its own transaction.
pub fn write_import(conn: &mut Connection, root_id: i64, imp: &FileImport) -> Result<()> {
    let tx = write_tx(conn)?;
    write_import_in(&tx, root_id, imp)?;
    tx.commit()?;
    Ok(())
}

struct ExistingTrack {
    id: i64,
    idx: i64,
    name: Option<String>,
    start_time: Option<i64>,
    edited: bool,
}

/// Pairs each new track with the catalog track it most likely is: same start time and
/// name, then same start time, then same position. Matching by position alone would move
/// titles and tags to another track when a changed file reorders or gains tracks.
fn match_tracks(existing: &[ExistingTrack], new: &[ProcessedTrack]) -> Vec<Option<usize>> {
    let mut used = vec![false; existing.len()];
    let mut out = vec![None; new.len()];
    for pass in 0..3 {
        for (k, t) in new.iter().enumerate() {
            if out[k].is_some() {
                continue;
            }
            let start = t.stats.start_time;
            let hit = existing.iter().enumerate().position(|(j, e)| {
                !used[j]
                    && match pass {
                        0 => start.is_some() && e.start_time == start && e.name == t.name,
                        1 => start.is_some() && e.start_time == start,
                        _ => e.idx == k as i64,
                    }
            });
            if let Some(j) = hit {
                used[j] = true;
                out[k] = Some(j);
            }
        }
    }
    out
}

/// Writes one imported file. User titles, descriptions, tags and activities stay with their
/// track; tracks edited in the app keep their edited geometry. A file that could not be
/// read keeps all its tracks: a read error on a flaky disk must not wipe the catalog.
pub fn write_import_in(tx: &Transaction, root_id: i64, imp: &FileImport) -> Result<()> {
    let file = match &imp.result {
        Ok(file) => file,
        Err(e) => {
            // parser_version 0 makes the next scan try this file again.
            tx.execute(
                "INSERT INTO files(root_id, path, size, mtime, hash, format, status, error, missing, scanned_at,
                   parser_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'error', ?7, 0, ?8, 0)
                 ON CONFLICT(path) DO UPDATE SET status = 'error', error = excluded.error, missing = 0,
                   scanned_at = excluded.scanned_at, parser_version = 0",
                params![root_id, imp.path, imp.size, imp.mtime, imp.hash, imp.format, e, now_ms()],
            )?;
            return Ok(());
        }
    };
    let file_id: i64 = tx.query_row(
        "INSERT INTO files(root_id, path, size, mtime, hash, format, status, error, missing, scanned_at,
           parser_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ok', NULL, 0, ?7, ?8)
         ON CONFLICT(path) DO UPDATE SET root_id = excluded.root_id, size = excluded.size,
           mtime = excluded.mtime, hash = excluded.hash, format = excluded.format,
           status = 'ok', error = NULL, missing = 0, scanned_at = excluded.scanned_at,
           parser_version = excluded.parser_version
         RETURNING id",
        params![root_id, imp.path, imp.size, imp.mtime, imp.hash, imp.format, now_ms(), PARSER_VERSION],
        |r| r.get(0),
    )?;
    write_waypoints(tx, file_id, &file.waypoints)?;
    // Saved geometry would not match the re-read tracks.
    crate::undo::clear_file(tx, file_id)?;

    let existing: Vec<ExistingTrack> = {
        let mut st = tx.prepare("SELECT id, idx, name, start_time, edited FROM tracks WHERE file_id = ?1 AND idx >= 0")?;
        st.query_map([file_id], |r| {
            Ok(ExistingTrack {
                id: r.get(0)?,
                idx: r.get(1)?,
                name: r.get(2)?,
                start_time: r.get(3)?,
                edited: r.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let matches = match_tracks(&existing, &file.tracks);
    // Park the existing tracks outside every real position so new positions can be
    // assigned without UNIQUE(file_id, idx) conflicts.
    for e in &existing {
        tx.execute("UPDATE tracks SET idx = ?2 WHERE id = ?1", params![e.id, PARKED_IDX - e.id])?;
    }
    for (k, t) in file.tracks.iter().enumerate() {
        match matches[k].map(|j| &existing[j]) {
            Some(e) if e.edited => {
                tx.execute("UPDATE tracks SET idx = ?2 WHERE id = ?1", params![e.id, k as i64])?;
            }
            Some(e) => {
                tx.execute("UPDATE tracks SET idx = ?2 WHERE id = ?1", params![e.id, k as i64])?;
                write_track(tx, e.id, t)?;
            }
            None => {
                tx.execute(
                    "INSERT INTO tracks(file_id, idx, point_count, distance_m) VALUES (?1, ?2, 0, 0)",
                    params![file_id, k as i64],
                )?;
                write_track(tx, tx.last_insert_rowid(), t)?;
            }
        }
    }
    let matched: Vec<usize> = matches.iter().flatten().copied().collect();
    for (j, e) in existing.iter().enumerate() {
        if matched.contains(&j) {
            continue;
        }
        if e.edited {
            // No longer in the file, but edited by the user: keep it like a split-off part.
            tx.execute(
                "UPDATE tracks SET idx = (SELECT MIN(0, MIN(idx)) FROM tracks WHERE file_id = ?2 AND idx > ?3) - 1
                 WHERE id = ?1",
                params![e.id, file_id, PARKED_IDX / 2],
            )?;
        } else {
            tx.execute("DELETE FROM tracks WHERE id = ?1", [e.id])?;
        }
    }
    delete_unused_tags(tx)?;
    Ok(())
}

/// Below every real or split-off position; parked rows only exist inside one transaction.
const PARKED_IDX: i64 = -(1 << 40);

fn write_track(tx: &Transaction, track_id: i64, t: &ProcessedTrack) -> Result<()> {
    let s = &t.stats;
    let bbox = s.bbox.map(|b| b.map(Some)).unwrap_or([None; 4]);
    tx.execute(
        "UPDATE tracks SET name = ?2, file_description = ?3, file_activity = ?4, start_time = ?5, end_time = ?6,
           point_count = ?7, distance_m = ?8, min_lat = ?9, min_lon = ?10, max_lat = ?11, max_lon = ?12,
           min_ele = ?13, max_ele = ?14
         WHERE id = ?1",
        params![
            track_id, t.name, t.description, t.activity, s.start_time, s.end_time, s.point_count as i64,
            s.distance_m, bbox[0], bbox[1], bbox[2], bbox[3], s.min_ele, s.max_ele
        ],
    )?;
    insert_track_data(tx, track_id, &t.blobs)?;
    write_places(tx, track_id, &t.places)?;
    Ok(())
}

pub fn insert_track_data(tx: &Transaction, track_id: i64, b: &crate::process::TrackBlobs) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO track_data(track_id, coords, ele, time, importance, segs, overview, file_segs, speed,
           course)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![track_id, b.coords, b.ele, b.time, b.importance, b.segs, b.overview, b.file_segs, b.speed, b.course],
    )?;
    Ok(())
}

pub fn write_places(conn: &Connection, track_id: i64, places: &[TrackPlace]) -> Result<()> {
    conn.execute("DELETE FROM track_places WHERE track_id = ?1", [track_id])?;
    let mut ins = conn.prepare_cached(
        "INSERT INTO track_places(track_id, seq, role, name, region, country) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (i, p) in places.iter().enumerate() {
        let Place { name, region, country } = &p.place;
        ins.execute(params![track_id, i as i64, p.role.as_str(), name, region, country])?;
    }
    conn.execute("UPDATE tracks SET places_version = ?2 WHERE id = ?1", params![track_id, PLACES_VERSION])?;
    Ok(())
}

/// Recomputes places whose `places_version` is outdated, from the stored overview line.
/// Returns the number of tracks updated.
pub fn refresh_places(conn: &mut Connection) -> Result<usize> {
    let index = places::index();
    // Reading inside the write lock keeps a concurrent edit or scan from being overwritten
    // with places computed from the geometry it just replaced.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: Vec<(i64, Vec<u8>)> = {
        let mut st = tx.prepare(
            "SELECT d.track_id, d.overview FROM track_data d JOIN tracks t ON t.id = d.track_id
             WHERE t.places_version < ?1",
        )?;
        st.query_map([PLACES_VERSION], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (id, overview) in &pending {
        write_places(&tx, *id, &index.track_places(&codec::decode_coords(overview)))?;
    }
    tx.commit()?;
    Ok(pending.len())
}

fn write_waypoints(tx: &rusqlite::Transaction, file_id: i64, waypoints: &[Waypoint]) -> Result<()> {
    tx.execute("DELETE FROM waypoints WHERE file_id = ?1", [file_id])?;
    let mut ins = tx.prepare(
        "INSERT INTO waypoints(file_id, idx, name, description, symbol, kind, lat, lon, ele, time)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for (i, w) in waypoints.iter().enumerate() {
        ins.execute(params![file_id, i as i64, w.name, w.description, w.symbol, w.kind, w.lat, w.lon, w.ele, w.time])?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaypointRow {
    pub id: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub symbol: Option<String>,
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    pub ele: Option<f64>,
    pub time: Option<i64>,
}

pub fn list_file_waypoints(conn: &Connection, file_id: i64) -> Result<Vec<WaypointRow>> {
    let mut st = conn.prepare(
        "SELECT id, name, description, symbol, kind, lat, lon, ele, time FROM waypoints
         WHERE file_id = ?1 ORDER BY idx",
    )?;
    let rows = st.query_map([file_id], |r| {
        Ok(WaypointRow {
            id: r.get(0)?,
            name: r.get(1)?,
            description: r.get(2)?,
            symbol: r.get(3)?,
            kind: r.get(4)?,
            lat: r.get(5)?,
            lon: r.get(6)?,
            ele: r.get(7)?,
            time: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Flags catalogued files that were not found on disk, like Lightroom's "missing" state.
pub fn update_missing(conn: &mut Connection, root_id: i64, seen: &[String]) -> Result<usize> {
    let tx = write_tx(conn)?;
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
    /// Start, places passed through, end.
    pub places: Vec<TrackPlace>,
    /// The track its bundle shows, when other files hold the same recording.
    pub bundle_main: Option<i64>,
    /// Other files' tracks that are the same recording, bundled or not.
    pub same_as: Vec<i64>,
    /// Longer tracks of other files this one lies within.
    pub within: Vec<i64>,
    /// The user shows this track on its own instead of in its bundle.
    pub bundle_apart: bool,
}

fn all_track_places(conn: &Connection) -> Result<HashMap<i64, Vec<TrackPlace>>> {
    let mut st = conn.prepare("SELECT track_id, role, name, region, country FROM track_places ORDER BY track_id, seq")?;
    let mut out: HashMap<i64, Vec<TrackPlace>> = HashMap::new();
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        out.entry(r.get(0)?).or_default().push(TrackPlace {
            role: Role::parse(&r.get::<_, String>(1)?),
            place: Place { name: r.get(2)?, region: r.get(3)?, country: r.get(4)? },
        });
    }
    Ok(out)
}

pub fn list_tracks(conn: &Connection) -> Result<Vec<TrackSummary>> {
    // One snapshot, so a sync on another connection cannot change the data between the queries.
    let snapshot = conn.unchecked_transaction()?;
    let conn = &*snapshot;
    let mut places = all_track_places(conn)?;
    let mut bundles = crate::dedup::bundles(conn)?;
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
        let id: i64 = r.get(0)?;
        Ok(TrackSummary {
            id,
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
            places: places.remove(&id).unwrap_or_default(),
            bundle_main: bundles.main.get(&id).copied(),
            same_as: bundles.same.remove(&id).unwrap_or_default(),
            within: bundles.within.remove(&id).unwrap_or_default(),
            bundle_apart: bundles.apart.contains(&id),
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
    /// Every line break (file segments plus glitch/gap splits); used for drawing.
    pub seg_starts: Vec<u32>,
    /// The file's own segments (or as edited); the structure shown and edited.
    pub file_seg_starts: Vec<u32>,
    /// Recorded per point, NaN where the file has none; see `Points`.
    pub speed: Vec<f32>,
    pub course: Vec<f32>,
}

pub fn load_track_data(conn: &Connection, track_id: i64) -> Result<TrackData> {
    Ok(conn.query_row(
        "SELECT coords, ele, time, importance, segs, COALESCE(file_segs, segs), speed, course
         FROM track_data WHERE track_id = ?1",
        [track_id],
        |r| {
            let coords = codec::decode_coords(&r.get::<_, Vec<u8>>(0)?);
            // Callers index these by point; a list of another length would panic under the write lock.
            let recorded = |k: usize| -> rusqlite::Result<Vec<f32>> {
                Ok(r.get::<_, Option<Vec<u8>>>(k)?
                    .map(|b| codec::decode_f32(&b))
                    .filter(|v| v.len() == coords.len())
                    .unwrap_or_else(|| vec![f32::NAN; coords.len()]))
            };
            let (speed, course) = (recorded(6)?, recorded(7)?);
            Ok(TrackData {
                coords,
                ele: codec::decode_f32(&r.get::<_, Vec<u8>>(1)?),
                time: codec::decode_i64(&r.get::<_, Vec<u8>>(2)?),
                importance: codec::decode_f32(&r.get::<_, Vec<u8>>(3)?),
                seg_starts: codec::decode_u32(&r.get::<_, Vec<u8>>(4)?),
                file_seg_starts: codec::decode_u32(&r.get::<_, Vec<u8>>(5)?),
                speed,
                course,
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
    let tx = write_tx(conn)?;
    tx.execute("DELETE FROM track_tags WHERE track_id = ?1", [id])?;
    for tag in tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        tx.execute("INSERT OR IGNORE INTO tags(name) VALUES (?1)", [tag])?;
        tx.execute(
            "INSERT OR IGNORE INTO track_tags(track_id, tag_id) SELECT ?1, id FROM tags WHERE name = ?2",
            params![id, tag],
        )?;
    }
    delete_unused_tags(&tx)?;
    tx.commit()?;
    Ok(())
}

pub fn list_tags(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT name FROM tags ORDER BY name COLLATE NOCASE")?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit;
    use crate::parse::Format;
    use crate::process::process_file;

    /// An untitled 10-point walk through Thun, imported into a fresh catalog.
    fn thun_catalog(tag: &str) -> (std::path::PathBuf, Connection, i64) {
        let dir = std::env::temp_dir().join(format!("gtm-places-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gpx = dir.join("thun.gpx");
        let pts: String = (0..10)
            .map(|i| {
                let lat = 46.758 + i as f64 * 0.0001;
                format!(r#"<trkpt lat="{lat:.4}" lon="7.6280"><time>2024-01-01T10:00:{i:02}Z</time></trkpt>"#)
            })
            .collect();
        std::fs::write(&gpx, format!("<gpx><trk><trkseg>{pts}</trkseg></trk></gpx>")).unwrap();
        let mut conn = open(&dir.join("c.sqlite")).unwrap();
        let root = add_root(&conn, dir.to_str().unwrap()).unwrap();
        write_import(&mut conn, root.id, &process_file(&gpx, Format::Gpx)).unwrap();
        let id = list_tracks(&conn).unwrap()[0].id;
        (dir, conn, id)
    }

    fn start_name(conn: &Connection, id: i64) -> Option<String> {
        let t = list_tracks(conn).unwrap().into_iter().find(|t| t.id == id).unwrap();
        t.places.into_iter().find(|p| p.role == Role::Start).map(|p| p.place.name)
    }

    fn places_version(conn: &Connection, id: i64) -> i64 {
        conn.query_row("SELECT places_version FROM tracks WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn import_and_edits_store_current_places() {
        let (dir, mut conn, id) = thun_catalog("edit");
        assert_eq!(start_name(&conn, id).as_deref(), Some("Thun"));
        assert_eq!(refresh_places(&mut conn).unwrap(), 0);

        edit::delete_points(&mut conn, id, &[0]).unwrap();
        let part = edit::split(&mut conn, id, 4).unwrap();
        for t in [id, part] {
            assert_eq!(start_name(&conn, t).as_deref(), Some("Thun"));
            assert_eq!(places_version(&conn, t), PLACES_VERSION);
        }
        // Untitled parts are named by their route, not "file.gpx (part 2)".
        assert_eq!(list_tracks(&conn).unwrap().iter().find(|t| t.id == part).unwrap().title, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn foreign_geometry_writes_are_refreshed() {
        let (dir, mut conn, id) = thun_catalog("refresh");
        // What a build without places does: rewrite the geometry and nothing else.
        conn.execute("DELETE FROM track_places", []).unwrap();
        conn.execute("UPDATE track_data SET overview = overview WHERE track_id = ?1", [id]).unwrap();
        assert_eq!(places_version(&conn, id), 0);
        assert_eq!(refresh_places(&mut conn).unwrap(), 1);
        assert_eq!(start_name(&conn, id).as_deref(), Some("Thun"));
        assert_eq!(refresh_places(&mut conn).unwrap(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn version_3_catalog_is_migrated_and_backfilled() {
        let (dir, conn, id) = thun_catalog("migrate");
        conn.execute_batch(
            "DROP TRIGGER track_data_insert_places; DROP TRIGGER track_data_update_places;
             DROP TABLE track_places; ALTER TABLE tracks DROP COLUMN places_version; PRAGMA user_version = 3;",
        )
        .unwrap();
        drop(conn);

        let mut conn = open(&dir.join("c.sqlite")).unwrap();
        assert_eq!(start_name(&conn, id), None);
        assert_eq!(refresh_places(&mut conn).unwrap(), 1);
        assert_eq!(start_name(&conn, id).as_deref(), Some("Thun"));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn setup(name: &str) -> (std::path::PathBuf, Connection, Root) {
        let dir = std::env::temp_dir().join(format!("gtm-cat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = open(&dir.join("c.sqlite")).unwrap();
        let root = add_root(&conn, dir.to_str().unwrap()).unwrap();
        (dir, conn, root)
    }

    fn trk(name: &str, minute: u32) -> String {
        format!(
            r#"<trk><name>{name}</name><trkseg><trkpt lat="50.0" lon="8.0"><time>2024-01-01T10:{minute:02}:00Z</time></trkpt><trkpt lat="50.001" lon="8.0"><time>2024-01-01T10:{minute:02}:30Z</time></trkpt></trkseg></trk>"#
        )
    }

    #[test]
    fn failed_reread_keeps_tracks_and_metadata() {
        let (dir, mut conn, root) = setup("err");
        let f = dir.join("a.gpx");
        std::fs::write(&f, format!("<gpx>{}</gpx>", trk("A", 1))).unwrap();
        write_import(&mut conn, root.id, &process_file(&f, Format::Gpx)).unwrap();
        let id = list_tracks(&conn).unwrap()[0].id;
        update_track_meta(&conn, id, Some("My title"), None).unwrap();
        set_track_tags(&mut conn, id, &["walk".into()]).unwrap();

        std::fs::write(&f, "<gpx><trk><trkseg><trkpt la").unwrap();
        write_import(&mut conn, root.id, &process_file(&f, Format::Gpx)).unwrap();
        let t = &list_tracks(&conn).unwrap()[0];
        assert_eq!((t.id, t.title.as_deref(), t.tags.clone()), (id, Some("My title"), vec!["walk".to_string()]));
        assert_eq!(known_files(&conn, root.id).unwrap().values().next().unwrap().parser_version, 0, "retried next scan");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reordered_tracks_keep_their_titles() {
        let (dir, mut conn, root) = setup("order");
        let f = dir.join("a.gpx");
        std::fs::write(&f, format!("<gpx>{}{}</gpx>", trk("A", 1), trk("B", 2))).unwrap();
        write_import(&mut conn, root.id, &process_file(&f, Format::Gpx)).unwrap();
        let b = list_tracks(&conn).unwrap().into_iter().find(|t| t.name.as_deref() == Some("B")).unwrap();
        update_track_meta(&conn, b.id, Some("Title of B"), None).unwrap();

        // A new track is inserted at the front and B moves to position 2.
        std::fs::write(&f, format!("<gpx>{}{}{}</gpx>", trk("New", 0), trk("A", 1), trk("B", 2))).unwrap();
        write_import(&mut conn, root.id, &process_file(&f, Format::Gpx)).unwrap();
        let tracks = list_tracks(&conn).unwrap();
        assert_eq!(tracks.len(), 3);
        let b2 = tracks.iter().find(|t| t.id == b.id).unwrap();
        assert_eq!((b2.name.as_deref(), b2.title.as_deref(), b2.idx), (Some("B"), Some("Title of B"), 2));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edited_track_survives_removal_from_file() {
        let (dir, mut conn, root) = setup("edited");
        let f = dir.join("a.gpx");
        std::fs::write(&f, format!("<gpx>{}{}</gpx>", trk("A", 1), trk("B", 2))).unwrap();
        write_import(&mut conn, root.id, &process_file(&f, Format::Gpx)).unwrap();
        let b = list_tracks(&conn).unwrap().into_iter().find(|t| t.name.as_deref() == Some("B")).unwrap();
        conn.execute("UPDATE tracks SET edited = 1 WHERE id = ?1", [b.id]).unwrap();

        std::fs::write(&f, format!("<gpx>{}</gpx>", trk("A", 1))).unwrap();
        write_import(&mut conn, root.id, &process_file(&f, Format::Gpx)).unwrap();
        let kept = list_tracks(&conn).unwrap().into_iter().find(|t| t.id == b.id).expect("edited track kept");
        assert!(kept.idx < 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overlapping_library_folders_are_refused() {
        let (dir, conn, _root) = setup("roots");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        assert!(add_root(&conn, dir.join("sub").to_str().unwrap()).is_err());
        assert!(add_root(&conn, dir.parent().unwrap().to_str().unwrap()).is_err());
        assert_eq!(add_root(&conn, dir.to_str().unwrap()).unwrap().id, list_roots(&conn).unwrap()[0].id);
        std::fs::remove_dir_all(&dir).ok();
    }
}
