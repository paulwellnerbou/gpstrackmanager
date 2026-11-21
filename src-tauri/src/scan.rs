use std::path::{Path, PathBuf};
use std::sync::mpsc::sync_channel;
use std::time::Instant;

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;
use walkdir::WalkDir;

use crate::catalog;
use crate::parse::{self, Format};
use crate::process::{PARSER_VERSION, file_mtime, process_file};

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub root_id: i64,
    pub phase: &'static str,
    pub total: usize,
    pub done: usize,
    pub current: Option<String>,
    pub tracks: usize,
    pub errors: usize,
    pub unsupported: usize,
    pub missing: usize,
    pub elapsed_ms: u128,
}

/// Parsing threads are capped: archives often live on USB disks where more readers only
/// add seeking, and each worker holds a whole file plus its decoded points in memory.
const WORKERS: usize = 4;

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|s| s.starts_with('.'))
}

/// Incremental scan: only new or changed files (by size and mtime) are parsed.
pub fn scan_root(db_path: &Path, root_id: i64, on_progress: &(dyn Fn(&ScanProgress) + Sync)) -> Result<ScanProgress> {
    let started = Instant::now();
    let mut conn = catalog::open(db_path)?;
    let root = catalog::get_root(&conn, root_id)?;
    let mut progress = ScanProgress { root_id, phase: "listing", ..Default::default() };
    on_progress(&progress);

    let known = catalog::known_files(&conn, root_id)?;
    let mut seen = Vec::new();
    let mut todo: Vec<(PathBuf, Format)> = Vec::new();
    let walker = WalkDir::new(&root.path).follow_links(false).into_iter();
    for entry in walker.filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name())) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        let path_str = path.to_string_lossy().into_owned();
        let (size, mtime) = (meta.len() as i64, file_mtime(&meta));
        let unchanged = known
            .get(&path_str)
            .is_some_and(|k| k.size == size && k.mtime == mtime && k.parser_version >= PARSER_VERSION);
        if let Some(format) = Format::from_path(path) {
            seen.push(path_str);
            if !unchanged {
                todo.push((path.to_path_buf(), format));
            }
        } else if parse::is_known_unsupported(path) {
            seen.push(path_str.clone());
            progress.unsupported += 1;
            if !unchanged {
                catalog::record_unsupported(&conn, root_id, &path_str, size, mtime)?;
            }
        }
    }

    progress.phase = "importing";
    progress.total = todo.len();
    on_progress(&progress);

    let pool = rayon::ThreadPoolBuilder::new().num_threads(WORKERS).build()?;
    let (tx, rx) = sync_channel(WORKERS);
    std::thread::scope(|s| -> Result<()> {
        s.spawn(|| {
            pool.install(|| {
                todo.par_iter().for_each_with(tx, |tx, (path, format)| {
                    let _ = tx.send(process_file(path, *format));
                });
            });
        });
        for imp in rx {
            match &imp.result {
                Ok(file) => progress.tracks += file.tracks.len(),
                Err(_) => progress.errors += 1,
            }
            // One file that cannot be written (e.g. database busy) must not end the whole scan.
            if let Err(e) = catalog::write_import(&mut conn, root_id, &imp) {
                eprintln!("cannot catalog {}: {e:#}", imp.path);
                progress.errors += 1;
            }
            progress.done += 1;
            progress.current = Some(imp.path);
            progress.elapsed_ms = started.elapsed().as_millis();
            on_progress(&progress);
        }
        Ok(())
    })?;

    progress.missing = catalog::update_missing(&mut conn, root_id, &seen)?;
    progress.phase = "done";
    progress.current = None;
    progress.elapsed_ms = started.elapsed().as_millis();
    on_progress(&progress);
    Ok(progress)
}
