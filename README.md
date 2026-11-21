# GPS Track Manager

Desktop app (Tauri 2: Rust core + React/MapLibre UI) for managing a large archive of GPS
tracks, Lightroom style: files stay where they are, a catalog indexes them.

## Run

```bash
npm install
npm run tauri dev
```

Headless import check against a folder (catalog file is created if missing):

```bash
cd src-tauri && cargo run --release --example scan -- /tmp/catalog.sqlite /path/to/tracks
```

## How it works

- **Catalog**: SQLite in the app data dir (`~/Library/Application Support/de.wellnerbou.gpstrackmanager/catalog.sqlite`).
  Library folders are scanned incrementally (size + mtime); points are stored as compact binary columns.
- **Huge tracks**: GPX is parsed streaming (`quick-xml`), never as a DOM. Each point stores its
  Douglas–Peucker importance, so any zoom level is one filter pass; the map only receives the
  points visible in the viewport at screen resolution.
- **Formats**: GPX, FIT. NMEA, KML/KMZ, TRL, ITM are listed as unsupported for now.
- **Devices**: mounted volumes with track files in known folders (`Garmin/Activity`, `GPX`, …).
  Imports copy into `<library>/<year>/`; the device is never modified.
- **Editing**: deleting points and splitting tracks change the catalog copy only. The original
  file is untouched, and “Revert to file” discards the edits.
- **Places**: start, end and the towns a track passes through are looked up offline against
  GeoNames places with ≥ 1000 inhabitants (`src-tauri/data/places.tsv.gz`, built by
  `examples/build_places.rs`). Place names are searchable and name tracks without a title.
- **Glitches**: jumps faster than 300 m/s and recording gaps (> 2 km after > 5 min) start a new
  segment, so they are neither drawn nor counted as distance.

## Layout

- `src-tauri/src/` — `parse/` (GPX, FIT), `lod.rs`, `process.rs`, `catalog.rs`, `scan.rs`,
  `device.rs`, `edit.rs`, `export.rs` (KML), `places.rs` (reverse geocoding), `lib.rs` (Tauri commands)
- `src-tauri/data/` — embedded place data (GeoNames, CC BY 4.0, https://www.geonames.org)
- `src/` — `App.tsx`, `components/` (Sidebar, MapView, Profile, Details, PointList)
