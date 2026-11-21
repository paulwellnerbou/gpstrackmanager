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
pub mod process;
pub mod scan;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;
use tauri::ipc::Response;
use tauri::{Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

struct AppState {
    db_path: PathBuf,
    db: Mutex<Connection>,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn f64_bytes(v: &[f64]) -> Response {
    Response::new(v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
}

#[tauri::command]
fn list_roots(state: State<AppState>) -> CmdResult<Vec<catalog::Root>> {
    catalog::list_roots(&state.db.lock().unwrap()).map_err(err)
}

#[tauri::command]
fn add_root(state: State<AppState>, path: String) -> CmdResult<catalog::Root> {
    catalog::add_root(&state.db.lock().unwrap(), &path).map_err(err)
}

#[tauri::command]
fn remove_root(state: State<AppState>, id: i64) -> CmdResult<()> {
    catalog::remove_root(&mut state.db.lock().unwrap(), id).map_err(err)
}

/// Runs in the background; progress arrives as `scan-progress` events.
#[tauri::command]
fn scan_root(app: tauri::AppHandle, state: State<AppState>, id: i64) -> CmdResult<()> {
    let db_path = state.db_path.clone();
    std::thread::spawn(move || {
        let emit = |p: &scan::ScanProgress| {
            let _ = app.emit("scan-progress", p);
        };
        if let Err(e) = scan::scan_root(&db_path, id, &emit) {
            let _ = app.emit("scan-error", e.to_string());
        }
    });
    Ok(())
}

#[tauri::command]
fn list_tracks(state: State<AppState>) -> CmdResult<Vec<catalog::TrackSummary>> {
    catalog::list_tracks(&state.db.lock().unwrap()).map_err(err)
}

#[tauri::command]
fn list_problem_files(state: State<AppState>) -> CmdResult<Vec<catalog::FileProblem>> {
    catalog::list_problem_files(&state.db.lock().unwrap()).map_err(err)
}

#[tauri::command]
fn get_overview(state: State<AppState>) -> CmdResult<Response> {
    catalog::overview(&state.db.lock().unwrap()).map(|v| f64_bytes(&v)).map_err(err)
}

/// `zoom` selects the level of detail, `bbox` = `[min_lon, min_lat, max_lon, max_lat]`.
#[tauri::command]
fn get_track_points(state: State<AppState>, id: i64, zoom: f64, bbox: Option<[f64; 4]>) -> CmdResult<Response> {
    let data = catalog::load_track_data(&state.db.lock().unwrap(), id).map_err(err)?;
    let tol = geo::pixel_size_at_zoom(zoom) as f32 * 0.5;
    Ok(f64_bytes(&geometry::select_points(&data, tol, bbox)))
}

#[tauri::command]
fn get_track_profile(state: State<AppState>, id: i64, buckets: usize) -> CmdResult<Response> {
    let data = catalog::load_track_data(&state.db.lock().unwrap(), id).map_err(err)?;
    Ok(f64_bytes(&geometry::profile(&data, buckets.clamp(10, 20_000))))
}

#[tauri::command]
fn get_track_point_table(state: State<AppState>, id: i64) -> CmdResult<Response> {
    let data = catalog::load_track_data(&state.db.lock().unwrap(), id).map_err(err)?;
    Ok(f64_bytes(&edit::point_table(&data)))
}

#[tauri::command]
fn delete_track_points(state: State<AppState>, id: i64, indices: Vec<u32>) -> CmdResult<()> {
    edit::delete_points(&mut state.db.lock().unwrap(), id, &indices).map_err(err)
}

/// Preview for `clean_up_track`.
#[tauri::command]
fn find_track_stays(state: State<AppState>, id: i64, radius_m: f64, min_duration_s: f64) -> CmdResult<Vec<cleanup::Stay>> {
    let data = catalog::load_track_data(&state.db.lock().unwrap(), id).map_err(err)?;
    let o = cleanup::StayOptions { radius_m, min_duration_s };
    Ok(cleanup::find_stays(&data.coords, &data.ele, &data.time, &data.file_seg_starts, o))
}

/// Returns the number of points removed.
#[tauri::command]
fn clean_up_track(state: State<AppState>, id: i64, radius_m: f64, min_duration_s: f64) -> CmdResult<usize> {
    let o = cleanup::StayOptions { radius_m, min_duration_s };
    edit::clean_up_stays(&mut state.db.lock().unwrap(), id, o).map_err(err)
}

#[tauri::command]
fn split_track(state: State<AppState>, id: i64, at: u32) -> CmdResult<i64> {
    edit::split(&mut state.db.lock().unwrap(), id, at).map_err(err)
}

#[tauri::command]
fn extract_track_range(state: State<AppState>, id: i64, start: u32, end: u32) -> CmdResult<i64> {
    edit::extract(&mut state.db.lock().unwrap(), id, start, end).map_err(err)
}

#[tauri::command]
fn join_track_segment(state: State<AppState>, id: i64, at: u32) -> CmdResult<()> {
    edit::join_segment(&mut state.db.lock().unwrap(), id, at).map_err(err)
}

#[tauri::command]
fn list_file_waypoints(state: State<AppState>, file_id: i64) -> CmdResult<Vec<catalog::WaypointRow>> {
    catalog::list_file_waypoints(&state.db.lock().unwrap(), file_id).map_err(err)
}

#[tauri::command]
fn revert_file(state: State<AppState>, file_id: i64) -> CmdResult<()> {
    edit::revert_file(&mut state.db.lock().unwrap(), file_id).map_err(err)
}

struct TrackRef {
    path: String,
    name: String,
    bbox: Option<[f64; 4]>,
}

fn track_ref(conn: &Connection, id: i64) -> Result<TrackRef, String> {
    conn.query_row(
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
    )
    .map_err(err)
}

/// Exports the catalog copy (including edits) as KML and opens it in Google Earth,
/// or in whatever app handles KML when Google Earth Pro is not installed.
#[tauri::command]
fn open_in_google_earth(app: tauri::AppHandle, state: State<AppState>, id: i64) -> CmdResult<()> {
    let (data, track) = {
        let conn = state.db.lock().unwrap();
        (catalog::load_track_data(&conn, id).map_err(err)?, track_ref(&conn, id)?)
    };
    let dir = app.path().app_cache_dir().map_err(err)?.join("export");
    std::fs::create_dir_all(&dir).map_err(err)?;
    let file = dir.join(format!("track-{id}.kml"));
    std::fs::write(&file, export::kml(&data, &track.name)).map_err(err)?;
    let file = file.to_string_lossy().into_owned();
    let opener = app.opener();
    opener
        .open_path(file.clone(), Some("Google Earth Pro"))
        .or_else(|_| opener.open_path(file, None::<&str>))
        .map_err(err)
}

/// Apple Maps cannot show a track line; it opens on the track's area.
#[tauri::command]
fn open_in_maps(app: tauri::AppHandle, state: State<AppState>, id: i64) -> CmdResult<()> {
    let track = track_ref(&state.db.lock().unwrap(), id)?;
    let [min_lat, min_lon, max_lat, max_lon] = track.bbox.ok_or("track has no position")?;
    let url = format!(
        "https://maps.apple.com/?ll={:.6},{:.6}&spn={:.6},{:.6}",
        (min_lat + max_lat) / 2.0,
        (min_lon + max_lon) / 2.0,
        (max_lat - min_lat).max(0.005) * 1.2,
        (max_lon - min_lon).max(0.005) * 1.2
    );
    app.opener().open_url(url, None::<&str>).map_err(err)
}

#[tauri::command]
fn reveal_track_file(app: tauri::AppHandle, state: State<AppState>, id: i64) -> CmdResult<()> {
    let track = track_ref(&state.db.lock().unwrap(), id)?;
    app.opener().reveal_item_in_dir(track.path).map_err(err)
}

#[tauri::command]
fn update_track_meta(state: State<AppState>, id: i64, title: Option<String>, description: Option<String>) -> CmdResult<()> {
    catalog::update_track_meta(&state.db.lock().unwrap(), id, title.as_deref(), description.as_deref()).map_err(err)
}

#[tauri::command]
fn set_track_activity(state: State<AppState>, id: i64, activity: Option<String>) -> CmdResult<()> {
    catalog::set_track_activity(&state.db.lock().unwrap(), id, activity.as_deref()).map_err(err)
}

#[tauri::command]
fn set_track_tags(state: State<AppState>, id: i64, tags: Vec<String>) -> CmdResult<()> {
    catalog::set_track_tags(&mut state.db.lock().unwrap(), id, &tags).map_err(err)
}

#[tauri::command]
fn list_tags(state: State<AppState>) -> CmdResult<Vec<String>> {
    catalog::list_tags(&state.db.lock().unwrap()).map_err(err)
}

#[tauri::command]
fn list_gps_devices(state: State<AppState>) -> CmdResult<Vec<device::GpsDevice>> {
    device::find_devices(Path::new("/Volumes"), &state.db.lock().unwrap()).map_err(err)
}

#[tauri::command]
fn import_device_files(state: State<AppState>, root_id: i64, files: Vec<String>) -> CmdResult<device::DeviceImportResult> {
    let mut conn = state.db.lock().unwrap();
    let root = catalog::get_root(&conn, root_id).map_err(err)?;
    device::import_files(&mut conn, &root, &files).map_err(err)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let db_path = app.path().app_data_dir()?.join("catalog.sqlite");
            let db = catalog::open(&db_path)?;
            app.manage(AppState { db_path, db: Mutex::new(db) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_roots,
            add_root,
            remove_root,
            scan_root,
            list_tracks,
            list_problem_files,
            get_overview,
            get_track_points,
            get_track_profile,
            get_track_point_table,
            delete_track_points,
            find_track_stays,
            clean_up_track,
            split_track,
            revert_file,
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
