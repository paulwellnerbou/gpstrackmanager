# GPS Track Manager

A desktop app for people with years of GPS tracks lying around in folders. Point it at those
folders and it builds a searchable library of every track. Your files stay where they are, and
nothing in them changes unless you ask for it.

![A day trip around Ao Nang and Krabi, Thailand: track on the map with distance, speed and altitude markers, the elevation profile below, and the track's details and points on the right](docs/screenshot.png)

## What you can do

- 📂 **Your whole archive in one library**: GPX, FIT, KML, NMEA and more
  ([Supported files](#supported-files)), straight from your folders. **Sync** picks up new and
  changed files.
- ⚡ **All tracks on one map at once**, still fast with thousands of tracks and millions of points.
- 🔎 **Search by place**: every track knows the towns it passes, so "Krabi" or "Thailand" finds it,
  even without a name.
- 📈 **Track details**: elevation profile, distance and speed markers, colouring by speed or
  altitude, and every single point.
- 🏷️ **Organise** with titles, descriptions, tags and activities.
- ✂️ **Edit**: delete points, split and join segments, undo with ⌘Z.
- 🧹 **Clean up** logger junk in one go: indoor scatter, 0/0 points, far-off spikes.
- 🔌 **Import from your GPS** (e.g. Garmin): new files are copied into your library, the device
  stays untouched.
- 🌍 **Open in Google Earth or Maps**, light, dark or black map themes.

## Your files are safe

All edits, titles, tags and descriptions live in the app's own catalog, not in your files. A track
file changes only when you choose **Write to file…**. Even then:

- the old file is kept next to it as `name.bak` (then `name.2.bak`, …),
- the new file is read back and checked against the catalog before it replaces anything,
- file permissions, Finder tags and the creation date are kept.

A file in any other format (FIT, KML, NMEA, …) is written as a new `.gpx` next to it. The
written file holds what you see in the app; anything the import skipped (such as sensor data)
stays in the backup.

## Where your data is stored

Titles, tags, edits and a fast copy of all points live in the app's own database:
`~/Library/Application Support/de.wellnerbou.gpstrackmanager/catalog.sqlite`. That keeps the map
fast and your files untouched. Back it up along with your tracks.

The catalog is not updated automatically: files you add or change outside the app show up after
you press **Sync**. Adding a folder and importing from a GPS device update it right away.

## Privacy

Your location history stays on your computer. Place names are looked up offline, from a list of
towns built into the app. What does go online: map tiles (from [OpenFreeMap](https://openfreemap.org)),
and the text you type into the place search on the map (sent to [Photon](https://photon.komoot.io)).

## Installing

The app is for macOS on Apple Silicon (M1 or newer).

1. Download the `.dmg` file of the
   [latest release](https://github.com/paulwellnerbou/gpstrackmanager/releases/latest), open it
   and drag **GPS Track Manager** to your Applications folder.
2. Open the app, click **Add folder…** and choose the folder with your tracks. The first scan of
   a large archive takes a while; later syncs only look at new and changed files.

### Building it yourself

On an Intel Mac, or to run the latest code, build the app from source. You need
[Node.js](https://nodejs.org), [Rust](https://rustup.rs) and the Xcode command line tools
(`xcode-select --install`).

```bash
git clone https://github.com/paulwellnerbou/gpstrackmanager.git
cd gpstrackmanager
npm install
npm run tauri build
```

The app is then in `src-tauri/target/release/bundle/macos/`.

## Supported files

- **GPX** (tracks, routes, waypoints) and **FIT** (Garmin activities, with laps).
- **KML** and **KMZ** (Google Earth): lines and `gx:Track` tracks become tracks, placemarks
  become waypoints. Holux ezTour KML stores a track's times as one placemark per point, named
  with the time but without a time zone; those times are read in your computer's time zone.
- **NMEA** logs (`RMC` and `GGA` sentences).
- **Holux** loggers such as the M-241: binary `.trl` track logs and ezTour `.itm` projects (all
  tracks and points of interest of the project).
- TCX, PLT, IGC and CSV files are found and listed as unsupported for now (the link under the
  library folder shows them).

## For developers

Tauri 2 app: Rust core (`src-tauri/`) with a React, TypeScript and MapLibre UI (`src/`).

```bash
npm install
npm run tauri dev
```

Headless import check against a folder (the catalog file is created if missing):

```bash
cd src-tauri && cargo run --release --example scan -- /tmp/catalog.sqlite /path/to/tracks
```

- **Catalog**: SQLite in the app data dir (`~/Library/Application Support/de.wellnerbou.gpstrackmanager/catalog.sqlite`).
  Library folders are scanned incrementally (size + mtime); points are stored as compact binary columns.
- **Huge tracks**: GPX and KML are parsed streaming (`quick-xml`), never as a DOM. Each point stores its
  Douglas–Peucker importance, so any zoom level is one filter pass; the map only receives the
  points visible in the viewport at screen resolution.
- **Devices**: mounted volumes with track files in known folders (`Garmin/Activity`, `GPX`, …).
  Imports copy into `<library>/<year>/`. A grown `Current.gpx` replaces its earlier copy, unless
  “Write to file…” wrote edits into that copy; then it is imported next to it.
- **Places**: GeoNames places with ≥ 1000 inhabitants (`src-tauri/data/places.tsv.gz`, built by
  `examples/build_places.rs`).
- **Glitches**: jumps faster than 300 m/s and recording gaps (> 2 km after > 5 min) start a new
  segment.
- **Layout**: `src-tauri/src/` holds `parse/` (GPX, FIT, KML/KMZ, NMEA, TRL, ITM), `lod.rs`, `process.rs`, `catalog.rs`,
  `scan.rs`, `device.rs`, `edit.rs`, `cleanup.rs`, `export.rs` (KML, GPX), `places.rs` and
  `lib.rs` (Tauri commands); `src/` holds `App.tsx` and `components/`.
- **App icon**: `src-tauri/icons/app-icon.svg`; regenerate the sizes with `npx tauri icon`.

## License

MIT, see [LICENSE](LICENSE). The embedded place data in `src-tauri/data/` is from
[GeoNames](https://www.geonames.org) and licensed under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/); see [NOTICE](NOTICE).
