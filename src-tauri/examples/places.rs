//! Fills in missing places in a catalog and prints them per track, for checking the matching:
//! `cargo run --release --example places -- <catalog.sqlite>`

use std::path::Path;
use std::time::Instant;

use gpstrackmanager_lib::catalog;
use gpstrackmanager_lib::places::Role;

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("usage: places <catalog.sqlite>");
    let mut conn = catalog::open(Path::new(&db))?;
    let started = Instant::now();
    let n = catalog::refresh_places(&mut conn)?;
    eprintln!("{n} tracks geocoded in {} ms", started.elapsed().as_millis());
    for t in catalog::list_tracks(&conn)? {
        let names =
            |role| t.places.iter().filter(|p| p.role == role).map(|p| p.place.name.as_str()).collect::<Vec<_>>();
        let file = Path::new(&t.file_path).file_name().unwrap_or_default().to_string_lossy();
        println!(
            "{file} | {} → {} | via {}",
            names(Role::Start).join(""),
            names(Role::End).join(""),
            names(Role::Via).join(", ")
        );
    }
    Ok(())
}
