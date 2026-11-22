import { invoke } from "@tauri-apps/api/core";

export interface Root {
  id: number;
  path: string;
}

export interface TrackSummary {
  id: number;
  fileId: number;
  filePath: string;
  format: string;
  idx: number;
  trackCountInFile: number;
  name: string | null;
  title: string | null;
  description: string | null;
  fileDescription: string | null;
  startTime: number | null;
  endTime: number | null;
  pointCount: number;
  distanceM: number;
  bbox: [number, number, number, number] | null; // minLat, minLon, maxLat, maxLon
  minEle: number | null;
  maxEle: number | null;
  tags: string[];
  missing: boolean;
  edited: boolean;
  /** User choice; null means "as in the file". */
  activity: string | null;
  fileActivity: string | null;
  /** Start, places passed through, end. */
  places: TrackPlace[];
  /** The track its bundle shows, when other files hold the same recording. */
  bundleMain: number | null;
  /** Other files' tracks that are the same recording, bundled or not. */
  sameAs: number[];
  /** Longer tracks of other files this one lies within. */
  within: number[];
  /** The user shows this track on its own instead of in its bundle. */
  bundleApart: boolean;
}

/** From GeoNames (CC BY 4.0), looked up offline. */
export interface TrackPlace {
  role: "start" | "via" | "end";
  name: string;
  region: string | null;
  country: string | null;
}

export const ACTIVITIES: { key: string; label: string; icon: string }[] = [
  { key: "walking", label: "Walking", icon: "🚶" },
  { key: "hiking", label: "Hiking", icon: "🥾" },
  { key: "running", label: "Running", icon: "🏃" },
  { key: "bike", label: "Bike", icon: "🚴" },
  { key: "mtb", label: "Mountain bike", icon: "🚵" },
  { key: "riding", label: "Riding", icon: "🐎" },
  { key: "car", label: "Car", icon: "🚗" },
  { key: "motorbike", label: "Motorbike", icon: "🏍️" },
  { key: "bus", label: "Bus", icon: "🚌" },
  { key: "train", label: "Train", icon: "🚆" },
  { key: "boat", label: "Boat", icon: "⛵" },
  { key: "aeroplane", label: "Aeroplane", icon: "✈️" },
  { key: "skiing", label: "Skiing", icon: "⛷️" },
  { key: "swimming", label: "Swimming", icon: "🏊" },
  { key: "other", label: "Other", icon: "📍" },
];

export function activityOf(t: TrackSummary) {
  const key = t.activity ?? t.fileActivity;
  return ACTIVITIES.find((a) => a.key === key) ?? null;
}

export interface WaypointRow {
  id: number;
  name: string | null;
  description: string | null;
  symbol: string | null;
  kind: "waypoint" | "lap";
  lat: number;
  lon: number;
  ele: number | null;
  time: number | null;
}

export interface PointTable {
  length: number;
  lat: Float64Array;
  lon: Float64Array;
  ele: Float64Array;
  /** Unix ms, NaN if unknown. */
  time: Float64Array;
  segStart: Uint8Array;
  /** Metres from the start, gaps between segments not counted. */
  dist: Float64Array;
  /** m/s calculated from the neighbouring points, NaN if unknown. */
  speed: Float64Array;
  /** Degrees clockwise from north calculated from the neighbouring points, NaN if unknown. */
  bearing: Float64Array;
  /** m/s as the file records it, NaN where it has none. */
  recordedSpeed: Float64Array;
  /** Course in degrees clockwise from north as the file records it, NaN where it has none. */
  recordedCourse: Float64Array;
}

/** The recorded value where the file has one, else the calculated one. */
export function pointSpeed(t: PointTable, i: number): { value: number; recorded: boolean } {
  const r = t.recordedSpeed[i];
  return Number.isNaN(r) ? { value: t.speed[i], recorded: false } : { value: r, recorded: true };
}

export function pointHeading(t: PointTable, i: number): { value: number; recorded: boolean } {
  const r = t.recordedCourse[i];
  return Number.isNaN(r) ? { value: t.bearing[i], recorded: false } : { value: r, recorded: true };
}

/** Where the logger lay still; `start` and `end` (inclusive) keep arrival and departure. */
export interface Stay {
  start: number;
  end: number;
  lat: number;
  lon: number;
  ele: number | null;
}

export interface CleanupPlan {
  stays: Stay[];
  /** Point indices. */
  spikes: number[];
}

export interface LastEdit {
  label: string;
  createdAt: number;
  /** The track the edit changed, not necessarily the one shown. */
  trackId: number;
}

export interface ScanProgress {
  rootId: number;
  phase: "listing" | "importing" | "done";
  total: number;
  done: number;
  current: string | null;
  tracks: number;
  errors: number;
  unsupported: number;
  missing: number;
  elapsedMs: number;
}

export interface FileProblem {
  path: string;
  status: string;
  error: string | null;
  missing: boolean;
}

export interface GpsDevice {
  volume: string;
  model: string | null;
  files: { path: string; name: string; size: number; mtime: number; imported: boolean }[];
}

export type Line = [number, number][];

/** A camera request for the map; `seq` makes repeated requests for the same place distinct. */
export type MapFocus =
  { bbox: [number, number, number, number]; seq: number } | { center: [number, number]; seq: number };

const f64 = (buf: ArrayBuffer) => new Float64Array(buf);

/** Splits a flat `[lon, lat, …]` list at NaN pairs into lines. */
export function toLines(v: Float64Array, start = 0, end = v.length): Line[] {
  const lines: Line[] = [];
  let cur: Line = [];
  for (let i = start; i < end; i += 2) {
    if (Number.isNaN(v[i])) {
      if (cur.length) lines.push(cur);
      cur = [];
    } else cur.push([v[i], v[i + 1]]);
  }
  if (cur.length) lines.push(cur);
  return lines;
}

// Commands run in parallel in the backend; metadata saves must still land in the order
// they were made, or an older save could overwrite a newer one.
let metaQueue: Promise<unknown> = Promise.resolve();
function inOrder<T>(f: () => Promise<T>): Promise<T> {
  const p = metaQueue.then(f, f);
  metaQueue = p.catch(() => undefined);
  return p;
}

export const api = {
  listRoots: () => invoke<Root[]>("list_roots"),
  addRoot: (path: string) => invoke<Root>("add_root", { path }),
  removeRoot: (id: number) => invoke<void>("remove_root", { id }),
  scanRoot: (id: number) => invoke<void>("scan_root", { id }),
  listTracks: () => invoke<TrackSummary[]>("list_tracks"),
  /** Finds places for tracks catalogued without them; resolves to the number of tracks updated. */
  refreshPlaces: () => invoke<number>("refresh_places"),
  /** Finds duplicates of new, re-read or edited tracks; resolves to how many were checked. */
  refreshMatches: () => invoke<number>("refresh_matches"),
  setBundleMain: (id: number) => invoke<void>("set_bundle_main", { id }),
  setBundleApart: (id: number, apart: boolean) => invoke<void>("set_bundle_apart", { id, apart }),
  listProblemFiles: () => invoke<FileProblem[]>("list_problem_files"),
  listTags: () => invoke<string[]>("list_tags"),
  updateTrackMeta: (id: number, title: string | null, description: string | null) =>
    inOrder(() => invoke<void>("update_track_meta", { id, title, description })),
  setTrackTags: (id: number, tags: string[]) => inOrder(() => invoke<void>("set_track_tags", { id, tags })),
  setTrackActivity: (id: number, activity: string | null) =>
    inOrder(() => invoke<void>("set_track_activity", { id, activity })),
  listGpsDevices: () => invoke<GpsDevice[]>("list_gps_devices"),
  importDeviceFiles: (rootId: number, files: string[]) =>
    invoke<{ copied: string[]; skipped: number; failed: string[] }>("import_device_files", { rootId, files }),

  deleteTrackPoints: (id: number, indices: number[]) => invoke<void>("delete_track_points", { id, indices }),
  /** What `cleanUpTrack` with the same options changes; no stays unless `radiusM` is given. */
  previewCleanUp: (id: number, radiusM: number | null, minDurationS: number, dropZero: boolean, dropSpikes: boolean) =>
    invoke<CleanupPlan>("preview_clean_up", { id, radiusM, minDurationS, dropZero, dropSpikes }),
  /**
   * Collapses stays unless `radiusM` is null; `dropZero` removes fixes at a 0 coordinate, `dropSpikes`
   * runs of points far off the track. Returns the number of points removed.
   */
  cleanUpTrack: (id: number, radiusM: number | null, minDurationS: number, dropZero: boolean, dropSpikes: boolean) =>
    invoke<number>("clean_up_track", { id, radiusM, minDurationS, dropZero, dropSpikes }),
  splitTrack: (id: number, at: number) => invoke<number>("split_track", { id, at }),
  revertFile: (fileId: number) => invoke<void>("revert_file", { fileId }),
  /** Writes the file's catalog version over it as GPX; `backup` is the previous file, renamed. */
  writeFileEdits: (fileId: number) => invoke<{ path: string; backup: string }>("write_file_edits", { fileId }),
  lastEdit: (fileId: number) => invoke<LastEdit | null>("last_edit", { fileId }),
  /** Undoes the file's newest point edit; `removed` is a split-off part that went away. */
  undoEdit: (fileId: number) =>
    invoke<{ restored: number[]; removed: number | null }>("undo_edit", { fileId }),
  extractTrackRange: (id: number, start: number, end: number) =>
    invoke<number>("extract_track_range", { id, start, end }),
  joinTrackSegment: (id: number, at: number) => invoke<void>("join_track_segment", { id, at }),
  listFileWaypoints: (fileId: number) => invoke<WaypointRow[]>("list_file_waypoints", { fileId }),
  openInGoogleEarth: (id: number) => invoke<void>("open_in_google_earth", { id }),
  openInMaps: (id: number) => invoke<void>("open_in_maps", { id }),
  revealTrackFile: (id: number) => invoke<void>("reveal_track_file", { id }),

  releaseTrackCache: () => invoke<void>("release_track_cache"),
  async pointTable(id: number): Promise<PointTable> {
    const v = f64(await invoke<ArrayBuffer>("get_track_point_table", { id }));
    const C = 10;
    const n = v.length / C;
    const col = (c: number) => {
      const out = new Float64Array(n);
      for (let i = 0; i < n; i++) out[i] = v[i * C + c];
      return out;
    };
    const segStart = new Uint8Array(n);
    for (let i = 0; i < n; i++) segStart[i] = v[i * C + 4];
    return {
      length: n,
      lat: col(0),
      lon: col(1),
      ele: col(2),
      time: col(3),
      segStart,
      dist: col(5),
      speed: col(6),
      bearing: col(7),
      recordedSpeed: col(8),
      recordedCourse: col(9),
    };
  },

  /** Map of track id → lines, from the pre-thinned overview data. */
  async overview(): Promise<Map<number, Line[]>> {
    const v = f64(await invoke<ArrayBuffer>("get_overview"));
    const out = new Map<number, Line[]>();
    let i = 0;
    while (i < v.length) {
      const id = v[i];
      const n = v[i + 1];
      const start = i + 2;
      const end = start + n * 2;
      out.set(id, toLines(v, start, end));
      i = end;
    }
    return out;
  },

  async trackPoints(id: number, zoom: number, bbox?: [number, number, number, number]): Promise<Line[]> {
    return toLines(f64(await invoke<ArrayBuffer>("get_track_points", { id, zoom, bbox: bbox ?? null })));
  },

  /** Indices of the points `trackPoints` would show, NaN between lines. */
  async trackPointIndices(id: number, zoom: number, bbox: [number, number, number, number]): Promise<Float64Array> {
    return f64(await invoke<ArrayBuffer>("get_track_point_indices", { id, zoom, bbox }));
  },

  /** Columns: distance (m), elevation (m), time (ms or NaN), lon, lat. */
  async trackProfile(id: number, buckets: number) {
    const v = f64(await invoke<ArrayBuffer>("get_track_profile", { id, buckets }));
    const n = v.length / 5;
    const cols = [0, 1, 2, 3, 4].map(() => new Float64Array(n));
    for (let i = 0; i < n; i++) for (let c = 0; c < 5; c++) cols[c][i] = v[i * 5 + c];
    return { dist: cols[0], ele: cols[1], time: cols[2], lon: cols[3], lat: cols[4] };
  },
};

export function startPlace(t: TrackSummary) {
  return t.places.find((p) => p.role === "start") ?? null;
}

export function endPlace(t: TrackSummary) {
  return t.places.find((p) => p.role === "end") ?? null;
}

/** "Munich → Garmisch-Partenkirchen", or one name for round trips. */
export function routeName(t: TrackSummary): string | null {
  const [a, b] = [startPlace(t)?.name, endPlace(t)?.name];
  if (a && b && a !== b) return `${a} → ${b}`;
  return a ?? b ?? null;
}

export function placeLabel(p: TrackPlace): string {
  return [p.name, p.region !== p.name ? p.region : null, p.country].filter(Boolean).join(", ");
}

export function displayName(t: TrackSummary): string {
  if (t.title) return t.title;
  if (t.name) return t.name;
  return routeName(t) ?? fileLabel(t);
}

/** Paths come from the OS, so Windows ones use backslashes. */
export function baseName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

/** The file name, numbered when the file holds several tracks. */
export function fileLabel(t: TrackSummary): string {
  const file = baseName(t.filePath);
  return t.trackCountInFile > 1 ? `${file} #${t.idx + 1}` : file;
}

/** The distinct files that hold a bundle; one file may hold several of its tracks. */
export function bundleFiles(tracks: TrackSummary[]): string[] {
  return [...new Set(tracks.map((t) => t.filePath))];
}

export function formatDistance(m: number): string {
  return m >= 10_000 ? `${(m / 1000).toFixed(0)} km` : m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${m.toFixed(0)} m`;
}

export function formatDate(ms: number | null): string {
  if (ms == null) return "—";
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function formatDuration(start: number | null, end: number | null): string {
  if (start == null || end == null || end <= start) return "—";
  const min = Math.round((end - start) / 60000);
  const h = Math.floor(min / 60);
  return h ? `${h} h ${min % 60} min` : `${min} min`;
}
