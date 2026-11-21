//! Headless scan for checking import speed and parser coverage:
//! `cargo run --release --example scan -- <catalog.sqlite> <folder>`

use std::path::Path;

use gpstrackmanager_lib::{catalog, scan};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (db, folder) = (Path::new(&args[1]), &args[2]);
    let conn = catalog::open(db)?;
    let root = catalog::add_root(&conn, folder)?;
    drop(conn);
    let last = scan::scan_root(db, root.id, &|p| {
        if p.phase != "importing" || p.done % 100 == 0 {
            eprintln!("{} {}/{} tracks={} errors={} {}ms", p.phase, p.done, p.total, p.tracks, p.errors, p.elapsed_ms);
        }
    })?;
    println!("{last:#?}");
    let conn = catalog::open(db)?;
    for f in catalog::list_problem_files(&conn)?.iter().filter(|f| f.status == "error") {
        println!("ERROR {}: {}", f.path, f.error.as_deref().unwrap_or(""));
    }
    Ok(())
}
