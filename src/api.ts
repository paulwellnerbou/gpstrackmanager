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
}

export interface PointTable {
  length: number;
  lat: Float64Array;
  lon: Float64Array;
  ele: Float64Array;
  time: Float64Array;
  segStart: Uint8Array;
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

export const api = {
  listRoots: () => invoke<Root[]>("list_roots"),
  addRoot: (path: string) => invoke<Root>("add_root", { path }),
  removeRoot: (id: number) => invoke<void>("remove_root", { id }),
  scanRoot: (id: number) => invoke<void>("scan_root", { id }),
  listTracks: () => invoke<TrackSummary[]>("list_tracks"),
  listProblemFiles: () => invoke<FileProblem[]>("list_problem_files"),
  listTags: () => invoke<string[]>("list_tags"),
  updateTrackMeta: (id: number, title: string | null, description: string | null) =>
    invoke<void>("update_track_meta", { id, title, description }),
  setTrackTags: (id: number, tags: string[]) => invoke<void>("set_track_tags", { id, tags }),
  listGpsDevices: () => invoke<GpsDevice[]>("list_gps_devices"),
  importDeviceFiles: (rootId: number, files: string[]) =>
    invoke<{ copied: string[]; skipped: number }>("import_device_files", { rootId, files }),

  deleteTrackPoints: (id: number, indices: number[]) => invoke<void>("delete_track_points", { id, indices }),
  splitTrack: (id: number, at: number) => invoke<number>("split_track", { id, at }),
  revertFile: (fileId: number) => invoke<void>("revert_file", { fileId }),
  openInGoogleEarth: (id: number) => invoke<void>("open_in_google_earth", { id }),
  openInMaps: (id: number) => invoke<void>("open_in_maps", { id }),
  revealTrackFile: (id: number) => invoke<void>("reveal_track_file", { id }),

  async pointTable(id: number): Promise<PointTable> {
    const v = f64(await invoke<ArrayBuffer>("get_track_point_table", { id }));
    const n = v.length / 5;
    const t: PointTable = {
      length: n,
      lat: new Float64Array(n),
      lon: new Float64Array(n),
      ele: new Float64Array(n),
      time: new Float64Array(n),
      segStart: new Uint8Array(n),
    };
    for (let i = 0; i < n; i++) {
      t.lat[i] = v[i * 5];
      t.lon[i] = v[i * 5 + 1];
      t.ele[i] = v[i * 5 + 2];
      t.time[i] = v[i * 5 + 3];
      t.segStart[i] = v[i * 5 + 4];
    }
    return t;
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

  /** Columns: distance (m), elevation (m), time (ms or NaN), lon, lat. */
  async trackProfile(id: number, buckets: number) {
    const v = f64(await invoke<ArrayBuffer>("get_track_profile", { id, buckets }));
    const n = v.length / 5;
    const cols = [0, 1, 2, 3, 4].map(() => new Float64Array(n));
    for (let i = 0; i < n; i++) for (let c = 0; c < 5; c++) cols[c][i] = v[i * 5 + c];
    return { dist: cols[0], ele: cols[1], time: cols[2], lon: cols[3], lat: cols[4] };
  },
};

export function displayName(t: TrackSummary): string {
  if (t.title) return t.title;
  if (t.name) return t.name;
  const file = t.filePath.split("/").pop() ?? t.filePath;
  return t.trackCountInFile > 1 ? `${file} #${t.idx + 1}` : file;
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
