pub mod activity;
pub mod catalog;
pub mod cleanup;
pub mod codec;
pub mod device;
pub mod edit;
pub mod export;
pub mod geo;
pub mod geometry;
pub mod lod;
pub mod model;
pub mod parse;
pub mod places;
pub mod process;
pub mod scan;
pub mod undo;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow, bail};
use rusqlite::Connection;
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

/// (track id, SQLite data_version, own write count)
type CacheKey = (i64, i64, u64);

struct AppState {
    db_path: PathBuf,
    db: Mutex<Connection>,
    /// Bumped by every write through `db`. SQLite's data_version only changes for other
    /// connections' commits (the scanner's), so both together tell whether a cached track is current.
    writes: AtomicU64,
    /// The selected track, so panning does not decode it from the database each time.
    track_cache: Mutex<Option<(CacheKey, Arc<catalog::TrackData>)>>,
    scanning: Arc<AtomicBool>,
    device_hashes: Mutex<device::HashCache>,
    /// Files the last device check found; the only ones an import accepts.
    device_files: Mutex<HashSet<String>>,
    /// Held for a whole device import: two imports at once would both find a file new and copy it twice.
    importing: Mutex<()>,
}

impl AppState {
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap()
    }

    fn write<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        let r = f(&mut conn);
        // Bumped before the lock is released, so no reader can pair the new data with the old count.
        self.writes.fetch_add(1, Ordering::SeqCst);
        r
    }

    fn track(&self, id: i64) -> Result<Arc<catalog::TrackData>> {
        let conn = self.conn();
        let version: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        let key = (id, version, self.writes.load(Ordering::SeqCst));
        let mut cache = self.track_cache.lock().unwrap();
        if let Some((k, data)) = &*cache
            && *k == key
        {
            return Ok(data.clone());
        }
        let data = Arc::new(catalog::load_track_data(&conn, id)?);
        *cache = Some((key, data.clone()));
        Ok(data)
    }
}

type CmdResult<T> = Result<T, String>;

/// Runs a command on the blocking pool: synchronous Tauri commands run on the main
/// thread and would freeze the window while they read files or the database.
async fn blocking<T: Send + 'static>(app: AppHandle, f: impl FnOnce(&AppState) -> Result<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(move || f(&app.state::<AppState>()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

fn f64_bytes(v: &[f64]) -> Response {
    Response::new(v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
}

#[tauri::command]
async fn list_roots(app: AppHandle) -> CmdResult<Vec<catalog::Root>> {
    blocking(app, |s| catalog::list_roots(&s.conn())).await
}

#[tauri::command]
async fn add_root(app: AppHandle, path: String) -> CmdResult<catalog::Root> {
    blocking(app, move |s| s.write(|c| catalog::add_root(c, &path))).await
}

#[tauri::command]
async fn remove_root(app: AppHandle, id: i64) -> CmdResult<()> {
    blocking(app, move |s| s.write(|c| catalog::remove_root(c, id))).await
}

/// Runs in the background; progress arrives as `scan-progress` events.
#[tauri::command]
async fn scan_root(app: AppHandle, id: i64) -> CmdResult<()> {
    let s = app.state::<AppState>();
    if s.scanning.swap(true, Ordering::SeqCst) {
        return Err("A sync or a write to a file is already running.".into());
    }
    let (db_path, scanning) = (s.db_path.clone(), s.scanning.clone());
    let app = app.clone();
    std::thread::spawn(move || {
        let emit = |p: &scan::ScanProgress| {
            let _ = app.emit("scan-progress", p);
        };
        let result = scan::scan_root(&db_path, id, &emit);
        scanning.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            let _ = app.emit("scan-error", format!("{e:#}"));
        }
    });
    Ok(())
}

/// Recomputes missing or outdated places; returns how many tracks changed.
#[tauri::command]
async fn refresh_places(app: AppHandle) -> CmdResult<usize> {
    // Its own connection: the long refresh must not hold the shared one.
    blocking(app, |s| catalog::refresh_places(&mut catalog::open(&s.db_path)?)).await
}

#[tauri::command]
async fn list_tracks(app: AppHandle) -> CmdResult<Vec<catalog::TrackSummary>> {
    blocking(app, |s| catalog::list_tracks(&s.conn())).await
}

#[tauri::command]
async fn list_problem_files(app: AppHandle) -> CmdResult<Vec<catalog::FileProblem>> {
    blocking(app, |s| catalog::list_problem_files(&s.conn())).await
}

#[tauri::command]
async fn get_overview(app: AppHandle) -> CmdResult<Response> {
    blocking(app, |s| catalog::overview(&s.conn()).map(|v| f64_bytes(&v))).await
}

/// `zoom` selects the level of detail, `bbox` = `[min_lon, min_lat, max_lon, max_lat]`.
#[tauri::command]
async fn get_track_points(app: AppHandle, id: i64, zoom: f64, bbox: Option<[f64; 4]>) -> CmdResult<Response> {
    blocking(app, move |s| {
        let data = s.track(id)?;
        let tol = geo::pixel_size_at_zoom(zoom) as f32 * 0.5;
        Ok(f64_bytes(&geometry::select_points(&data, tol, bbox)))
    })
    .await
}

/// Like `get_track_points`, but the indices of the shown points (NaN between lines), so the
/// UI can colour the line from its point table.
#[tauri::command]
async fn get_track_point_indices(app: AppHandle, id: i64, zoom: f64, bbox: Option<[f64; 4]>) -> CmdResult<Response> {
    blocking(app, move |s| {
        let data = s.track(id)?;
        let tol = geo::pixel_size_at_zoom(zoom) as f32 * 0.5;
        let idx: Vec<f64> =
            geometry::select_indices(&data, tol, bbox).into_iter().map(|i| i.map_or(f64::NAN, |i| i as f64)).collect();
        Ok(f64_bytes(&idx))
    })
    .await
}

#[tauri::command]
async fn get_track_profile(app: AppHandle, id: i64, buckets: usize) -> CmdResult<Response> {
    blocking(app, move |s| Ok(f64_bytes(&geometry::profile(&*s.track(id)?, buckets.clamp(10, 20_000))))).await
}

#[tauri::command]
async fn get_track_point_table(app: AppHandle, id: i64) -> CmdResult<Response> {
    blocking(app, move |s| Ok(f64_bytes(&edit::point_table(&*s.track(id)?)))).await
}

/// Frees the cached track once nothing is selected; a large one holds tens of MB.
#[tauri::command]
async fn release_track_cache(app: AppHandle) -> CmdResult<()> {
    blocking(app, |s| {
        *s.track_cache.lock().unwrap() = None;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn delete_track_points(app: AppHandle, id: i64, indices: Vec<u32>) -> CmdResult<()> {
    blocking(app, move |s| s.write(|c| edit::delete_points(c, id, &indices))).await
}

/// What `clean_up_track` with the same options changes: the stays it collapses (none
/// unless `radius_m` is given) and the spikes it removes.
#[tauri::command]
async fn preview_clean_up(
    app: AppHandle,
    id: i64,
    radius_m: Option<f64>,
    min_duration_s: f64,
    drop_zero: bool,
    drop_spikes: bool,
) -> CmdResult<cleanup::Plan> {
    let o = radius_m.map(|radius_m| cleanup::StayOptions { radius_m, min_duration_s });
    blocking(app, move |s| {
        let d = s.track(id)?;
        Ok(cleanup::plan(&d.coords, &d.ele, &d.time, &d.file_seg_starts, o, drop_zero, drop_spikes))
    })
    .await
}

/// Collapses stays unless `radius_m` is None; with `drop_zero` and `drop_spikes`, also
/// removes fixes at a 0 coordinate and spikes. Returns the number of points removed.
#[tauri::command]
async fn clean_up_track(
    app: AppHandle,
    id: i64,
    radius_m: Option<f64>,
    min_duration_s: f64,
    drop_zero: bool,
    drop_spikes: bool,
) -> CmdResult<usize> {
    let o = radius_m.map(|radius_m| cleanup::StayOptions { radius_m, min_duration_s });
    blocking(app, move |s| s.write(|c| edit::clean_up(c, id, o, drop_zero, drop_spikes))).await
}

#[tauri::command]
async fn split_track(app: AppHandle, id: i64, at: u32) -> CmdResult<i64> {
    blocking(app, move |s| s.write(|c| edit::split(c, id, at))).await
}

#[tauri::command]
async fn extract_track_range(app: AppHandle, id: i64, start: u32, end: u32) -> CmdResult<i64> {
    blocking(app, move |s| s.write(|c| edit::extract(c, id, start, end))).await
}

#[tauri::command]
async fn join_track_segment(app: AppHandle, id: i64, at: u32) -> CmdResult<()> {
    blocking(app, move |s| s.write(|c| edit::join_segment(c, id, at))).await
}

#[tauri::command]
async fn last_edit(app: AppHandle, file_id: i64) -> CmdResult<Option<undo::LastEdit>> {
    blocking(app, move |s| undo::last_edit(&s.conn(), file_id)).await
}

#[tauri::command]
async fn undo_edit(app: AppHandle, file_id: i64) -> CmdResult<undo::UndoResult> {
    blocking(app, move |s| s.write(|c| undo::undo(c, file_id))).await
}

#[tauri::command]
async fn list_file_waypoints(app: AppHandle, file_id: i64) -> CmdResult<Vec<catalog::WaypointRow>> {
    blocking(app, move |s| catalog::list_file_waypoints(&s.conn(), file_id)).await
}

#[tauri::command]
async fn revert_file(app: AppHandle, file_id: i64) -> CmdResult<()> {
    blocking(app, move |s| s.write(|c| edit::revert_file(c, file_id))).await
}

/// Not during a sync, which reads files before it catalogs them and would store the file
/// as it was; a sync started meanwhile is refused. Device imports wait, as one may replace
/// a library copy of the device's track log.
#[tauri::command]
async fn write_file_edits(app: AppHandle, file_id: i64) -> CmdResult<edit::WrittenFile> {
    let scanning = app.state::<AppState>().scanning.clone();
    if scanning.swap(true, Ordering::SeqCst) {
        return Err("A sync is running. Write to the file once it has finished.".into());
    }
    let result = blocking(app, move |s| {
        let _importing = s.importing.lock().unwrap();
        s.write(|c| edit::write_to_file(c, file_id))
    })
    .await;
    scanning.store(false, Ordering::SeqCst);
    result
}

struct TrackRef {
    path: String,
    name: String,
    bbox: Option<[f64; 4]>,
}

fn track_ref(conn: &Connection, id: i64) -> Result<TrackRef> {
    Ok(conn.query_row(
        "SELECT f.path, COALESCE(t.title, t.name), t.min_lat, t.min_lon, t.max_lat, t.max_lon
         FROM tracks t JOIN files f ON f.id = t.file_id WHERE t.id = ?1",
        [id],
        |r| {
            let path: String = r.get(0)?;
            let name = r.get::<_, Option<String>>(1)?.unwrap_or_else(|| {
                Path::new(&path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
            });
            let bbox = match (r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?) {
                (Some(a), Some(b), Some(c), Some(d)) => Some([a, b, c, d]),
                _ => None,
            };
            Ok(TrackRef { path, name, bbox })
        },
    )?)
}

/// Exports the catalog copy (including edits) as KML and opens it in Google Earth,
/// or in whatever app handles KML when Google Earth Pro is not installed.
#[tauri::command]
async fn open_in_google_earth(app: AppHandle, id: i64) -> CmdResult<()> {
    let handle = app.clone();
    blocking(app, move |s| {
        let data = s.track(id)?;
        let track = track_ref(&s.conn(), id)?;
        let dir = handle.path().app_cache_dir()?.join("export");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!("track-{id}.kml"));
        std::fs::write(&file, export::kml(&data, &track.name))?;
        let file = file.to_string_lossy().into_owned();
        let opener = handle.opener();
        opener.open_path(file.clone(), Some("Google Earth Pro")).or_else(|_| opener.open_path(file, None::<&str>))?;
        Ok(())
    })
    .await
}

/// Apple Maps cannot show a track line; it opens on the track's area.
#[tauri::command]
async fn open_in_maps(app: AppHandle, id: i64) -> CmdResult<()> {
    let handle = app.clone();
    blocking(app, move |s| {
        let track = track_ref(&s.conn(), id)?;
        let [min_lat, min_lon, max_lat, max_lon] = track.bbox.ok_or_else(|| anyhow!("track has no position"))?;
        let url = format!(
            "https://maps.apple.com/?ll={:.6},{:.6}&spn={:.6},{:.6}",
            (min_lat + max_lat) / 2.0,
            (min_lon + max_lon) / 2.0,
            (max_lat - min_lat).max(0.005) * 1.2,
            (max_lon - min_lon).max(0.005) * 1.2
        );
        Ok(handle.opener().open_url(url, None::<&str>)?)
    })
    .await
}

#[tauri::command]
async fn reveal_track_file(app: AppHandle, id: i64) -> CmdResult<()> {
    let handle = app.clone();
    blocking(app, move |s| {
        let track = track_ref(&s.conn(), id)?;
        Ok(handle.opener().reveal_item_in_dir(track.path)?)
    })
    .await
}

#[tauri::command]
async fn update_track_meta(app: AppHandle, id: i64, title: Option<String>, description: Option<String>) -> CmdResult<()> {
    blocking(app, move |s| s.write(|c| catalog::update_track_meta(c, id, title.as_deref(), description.as_deref()))).await
}

#[tauri::command]
async fn set_track_activity(app: AppHandle, id: i64, activity: Option<String>) -> CmdResult<()> {
    blocking(app, move |s| s.write(|c| catalog::set_track_activity(c, id, activity.as_deref()))).await
}

#[tauri::command]
async fn set_track_tags(app: AppHandle, id: i64, tags: Vec<String>) -> CmdResult<()> {
    blocking(app, move |s| s.write(|c| catalog::set_track_tags(c, id, &tags))).await
}

#[tauri::command]
async fn list_tags(app: AppHandle) -> CmdResult<Vec<String>> {
    blocking(app, |s| catalog::list_tags(&s.conn())).await
}

#[tauri::command]
async fn list_gps_devices(app: AppHandle) -> CmdResult<Vec<device::GpsDevice>> {
    blocking(app, |s| {
        let (roots, known) = {
            let conn = s.conn();
            (catalog::list_roots(&conn)?, catalog::all_hashes(&conn)?)
        };
        let devices = device::find_devices(Path::new("/Volumes"), &roots, &known, &mut s.device_hashes.lock().unwrap())?;
        *s.device_files.lock().unwrap() = devices.iter().flat_map(|d| d.files.iter().map(|f| f.path.clone())).collect();
        Ok(devices)
    })
    .await
}

#[tauri::command]
async fn import_device_files(app: AppHandle, root_id: i64, files: Vec<String>) -> CmdResult<device::DeviceImportResult> {
    blocking(app, move |s| {
        {
            let allowed = s.device_files.lock().unwrap();
            if let Some(f) = files.iter().find(|f| !allowed.contains(*f)) {
                bail!("{f} is not a file on a detected GPS device");
            }
        }
        let _importing = s.importing.lock().unwrap();
        let root = catalog::get_root(&s.conn(), root_id)?;
        device::import_files(&s.db, &root, &files, || {
            s.writes.fetch_add(1, Ordering::SeqCst);
        })
    })
    .await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let db_path = app.path().app_data_dir()?.join("catalog.sqlite");
            let db = catalog::open(&db_path)?;
            app.manage(AppState {
                db_path,
                db: Mutex::new(db),
                writes: AtomicU64::new(0),
                track_cache: Mutex::new(None),
                scanning: Arc::new(AtomicBool::new(false)),
                device_hashes: Default::default(),
                device_files: Default::default(),
                importing: Mutex::new(()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_roots,
            add_root,
            remove_root,
            scan_root,
            list_tracks,
            refresh_places,
            list_problem_files,
            get_overview,
            get_track_points,
            get_track_point_indices,
            get_track_profile,
            get_track_point_table,
            release_track_cache,
            delete_track_points,
            preview_clean_up,
            clean_up_track,
            split_track,
            revert_file,
            write_file_edits,
            last_edit,
            undo_edit,
            extract_track_range,
            join_track_segment,
            list_file_waypoints,
            open_in_google_earth,
            open_in_maps,
            reveal_track_file,
            update_track_meta,
            set_track_tags,
            set_track_activity,
            list_tags,
            list_gps_devices,
            import_device_files,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
