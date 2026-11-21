//! Undo for point edits. Before an edit changes a track, its geometry is saved; undoing
//! puts it back and removes the track the edit created (a split-off part). Each file has
//! its own history, undone newest first, so a later edit of a split-off part is always
//! undone before the split that created it.

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;

use crate::catalog;

/// Older edits of a file are forgotten; each entry holds whole tracks.
const MAX_EDITS_PER_FILE: i64 = 20;

/// Geometry columns of `tracks` that point edits change.
const TRACK_COLUMNS: &str =
    "start_time, end_time, point_count, distance_m, min_lat, min_lon, max_lat, max_lon, min_ele, max_ele, edited";
const DATA_COLUMNS: &str = "coords, ele, time, importance, segs, overview, file_segs";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastEdit {
    pub label: String,
    pub created_at: i64,
    /// The track the edit changed; with several tracks in a file it may not be the one shown.
    pub track_id: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoResult {
    /// Tracks whose points were put back.
    pub restored: Vec<i64>,
    /// The split-off part that was removed, if the edit created one.
    pub removed: Option<i64>,
}

pub fn file_of(tx: &Transaction, track_id: i64) -> Result<i64> {
    Ok(tx.query_row("SELECT file_id FROM tracks WHERE id = ?1", [track_id], |r| r.get(0))?)
}

/// Saves `tracks` as they are now, before an edit labelled `label` changes them.
/// `created` is a track the edit inserts; undo removes it.
pub fn record(tx: &Transaction, label: &str, tracks: &[i64], created: Option<i64>) -> Result<()> {
    let Some(&first) = tracks.first() else { bail!("nothing to record") };
    let file_id = file_of(tx, first)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64;
    tx.execute(
        "INSERT INTO edit_undo(file_id, label, created_at, created_track_id) VALUES (?1, ?2, ?3, ?4)",
        params![file_id, label, now, created],
    )?;
    let undo_id = tx.last_insert_rowid();
    for &t in tracks {
        tx.execute(
            &format!(
                "INSERT INTO edit_undo_tracks(undo_id, track_id, {TRACK_COLUMNS}, {DATA_COLUMNS})
                 SELECT ?1, t.id, {}, {}
                 FROM tracks t JOIN track_data d ON d.track_id = t.id WHERE t.id = ?2",
                prefixed("t", TRACK_COLUMNS),
                prefixed("d", DATA_COLUMNS)
            ),
            params![undo_id, t],
        )?;
    }
    tx.execute(
        "DELETE FROM edit_undo WHERE file_id = ?1 AND id NOT IN
           (SELECT id FROM edit_undo WHERE file_id = ?1 ORDER BY id DESC LIMIT ?2)",
        params![file_id, MAX_EDITS_PER_FILE],
    )?;
    Ok(())
}

fn prefixed(alias: &str, columns: &str) -> String {
    columns.split(", ").map(|c| format!("{alias}.{c}")).collect::<Vec<_>>().join(", ")
}

/// Forgets a file's history; its tracks were replaced from the file.
pub fn clear_file(tx: &Transaction, file_id: i64) -> Result<()> {
    tx.execute("DELETE FROM edit_undo WHERE file_id = ?1", [file_id])?;
    Ok(())
}

pub fn last_edit(conn: &Connection, file_id: i64) -> Result<Option<LastEdit>> {
    Ok(conn
        .query_row(
            "SELECT e.label, e.created_at, (SELECT MIN(track_id) FROM edit_undo_tracks WHERE undo_id = e.id)
             FROM edit_undo e WHERE e.file_id = ?1 ORDER BY e.id DESC LIMIT 1",
            [file_id],
            |r| Ok(LastEdit { label: r.get(0)?, created_at: r.get(1)?, track_id: r.get(2)? }),
        )
        .optional()?)
}

/// Undoes the newest edit of the file.
pub fn undo(conn: &mut Connection, file_id: i64) -> Result<UndoResult> {
    let tx = catalog::write_tx(conn)?;
    let Some((undo_id, created)) = tx
        .query_row(
            "SELECT id, created_track_id FROM edit_undo WHERE file_id = ?1 ORDER BY id DESC LIMIT 1",
            [file_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .optional()?
    else {
        bail!("Nothing to undo.");
    };
    let restored: Vec<i64> = {
        let mut st = tx.prepare("SELECT track_id FROM edit_undo_tracks WHERE undo_id = ?1")?;
        st.query_map([undo_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for &t in &restored {
        let set = |columns: &str| columns.split(", ").map(|c| format!("{c} = u.{c}")).collect::<Vec<_>>().join(", ");
        let changed = tx.execute(
            &format!(
                "UPDATE tracks SET {} FROM edit_undo_tracks u WHERE u.undo_id = ?1 AND u.track_id = ?2 AND tracks.id = ?2",
                set(TRACK_COLUMNS)
            ),
            params![undo_id, t],
        )?;
        if changed == 0 {
            bail!("The track changed since this edit and cannot be restored.");
        }
        tx.execute(
            &format!(
                "UPDATE track_data SET {} FROM edit_undo_tracks u
                 WHERE u.undo_id = ?1 AND u.track_id = ?2 AND track_data.track_id = ?2",
                set(DATA_COLUMNS)
            ),
            params![undo_id, t],
        )?;
    }
    if let Some(c) = created {
        tx.execute("DELETE FROM tracks WHERE id = ?1 AND file_id = ?2", params![c, file_id])?;
        catalog::delete_unused_tags(&tx)?;
    }
    tx.execute("DELETE FROM edit_undo WHERE id = ?1", [undo_id])?;
    tx.commit()?;
    Ok(UndoResult { restored, removed: created })
}
