import { Marker, type Map as MapLibreMap } from "maplibre-gl";
import type { PointTable, WaypointRow } from "../api";

export interface MarkerOptions {
  /** How the selected track is drawn: as a line, as its points, or both. */
  pointView: "line" | "points" | "both";
  startEnd: boolean;
  waypoints: boolean;
  distance: boolean;
  /** Which values the labels show. */
  showDistance: boolean;
  showSpeed: boolean;
  showAltitude: boolean;
  showTime: boolean;
}

export const DEFAULT_MARKER_OPTIONS: MarkerOptions = {
  pointView: "line",
  startEnd: true,
  waypoints: true,
  distance: true,
  showDistance: true,
  showSpeed: false,
  showAltitude: false,
  showTime: false,
};

/** Target screen spacing of distance markers; the interval is the next "nice" step above. */
const SPACING_PX = 120;
const STEPS_M = [10, 20, 50, 100, 200, 500, 1e3, 2e3, 5e3, 1e4, 2e4, 5e4, 1e5, 2e5, 5e5, 1e6];
/** DOM markers are cheap only in small numbers. */
const MAX_DISTANCE_MARKERS = 200;
const MAX_SELECTION_LABELS = 40;
const MAX_WAYPOINTS = 300;

const ARROW_SVG =
  '<svg viewBox="0 0 16 16" width="16" height="16"><path d="M8 1 L14 14 L8 10.5 L2 14 Z" fill="#f08c00" stroke="#fff" stroke-width="1.5" stroke-linejoin="round"/></svg>';

function formatKm(m: number) {
  const km = m / 1000;
  return `${km.toLocaleString(undefined, { maximumFractionDigits: km < 10 ? 1 : 0 })} km`;
}

function pad(n: number) {
  return String(n).padStart(2, "0");
}

function label(t: PointTable, i: number, o: MarkerOptions, distanceOverride?: number): string {
  const parts: string[] = [];
  if (o.showDistance) parts.push(formatKm(distanceOverride ?? t.dist[i]));
  if (o.showSpeed && !Number.isNaN(t.speed[i])) parts.push(`${Math.round(t.speed[i] * 3.6)} km/h`);
  if (o.showAltitude && !Number.isNaN(t.ele[i])) parts.push(`${Math.round(t.ele[i])} m`);
  if (o.showTime && !Number.isNaN(t.time[i])) {
    const d = new Date(t.time[i]);
    parts.push(`${pad(d.getHours())}:${pad(d.getMinutes())}`);
  }
  return parts.join(" · ");
}

/** Last index whose cumulative distance is <= d (dist is non-decreasing). */
function indexAtDistance(dist: Float64Array, d: number): number {
  let lo = 0;
  let hi = dist.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (dist[mid] <= d) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

/**
 * Start/end, distance and selection markers for the selected track only. Markers are
 * rebuilt after each map move, limited to the viewport, so their number stays small
 * whatever the size of the track.
 */
export class TrackMarkers {
  private markers: Marker[] = [];
  private table: PointTable | null = null;
  /** Selected points that get a label; computed per selection, not per map move. */
  private labelPicks: number[] = [];
  private options = DEFAULT_MARKER_OPTIONS;
  private waypoints: WaypointRow[] = [];
  private focusedWaypoint: number | null = null;

  constructor(private map: MapLibreMap) {
    map.on("moveend", () => this.render());
  }

  update(
    table: PointTable | null,
    selection: number[],
    options: MarkerOptions,
    waypoints: WaypointRow[],
    focusedWaypoint: number | null,
  ) {
    this.waypoints = waypoints;
    this.focusedWaypoint = focusedWaypoint;
    this.table = table;
    this.labelPicks = runEnds(selection);
    this.options = options;
    this.render();
  }

  private clear() {
    for (const m of this.markers) m.remove();
    this.markers = [];
  }

  private add(el: HTMLElement, lon: number, lat: number) {
    this.markers.push(new Marker({ element: el, anchor: "center" }).setLngLat([lon, lat]).addTo(this.map));
  }

  private pin(kind: "start" | "end", text: string) {
    const el = document.createElement("div");
    el.className = `trk-pin trk-pin-${kind}`;
    el.textContent = kind === "start" ? "S" : "E";
    el.title = text;
    return el;
  }

  private arrow(bearing: number, text: string, kind: "distance" | "selection") {
    const el = document.createElement("div");
    el.className = `trk-mk trk-mk-${kind}`;
    if (!Number.isNaN(bearing)) {
      const a = document.createElement("div");
      a.className = "trk-arrow";
      a.style.transform = `rotate(${bearing - this.map.getBearing()}deg)`;
      a.innerHTML = ARROW_SVG;
      el.appendChild(a);
    } else {
      el.classList.add("trk-mk-dot");
    }
    if (text) {
      const l = document.createElement("div");
      l.className = "trk-label";
      l.textContent = text;
      el.appendChild(l);
    }
    return el;
  }

  private waypoint(w: WaypointRow, focused: boolean) {
    const el = document.createElement("div");
    el.className = `trk-wp trk-wp-${w.kind}${focused ? " focused" : ""}`;
    const when = w.time != null ? ` · ${new Date(w.time).toLocaleString()}` : "";
    el.title = `${w.name ?? "Waypoint"}${when}${w.description ? `\n${w.description}` : ""}`;
    if (focused) {
      const l = document.createElement("div");
      l.className = "trk-label";
      l.textContent = w.name ?? "Waypoint";
      el.appendChild(l);
    }
    return el;
  }

  render() {
    this.clear();
    const o = this.options;
    const b = this.map.getBounds();
    const padX = (b.getEast() - b.getWest()) * 0.1;
    const padY = (b.getNorth() - b.getSouth()) * 0.1;
    const visible = (lon: number, lat: number) =>
      lon >= b.getWest() - padX &&
      lon <= b.getEast() + padX &&
      lat >= b.getSouth() - padY &&
      lat <= b.getNorth() + padY;

    // The focused waypoint is shown even with waypoints switched off: it was picked explicitly.
    let shown = 0;
    for (const w of this.waypoints) {
      const focused = w.id === this.focusedWaypoint;
      if (!focused && (!o.waypoints || shown >= MAX_WAYPOINTS)) continue;
      if (!visible(w.lon, w.lat)) continue;
      this.add(this.waypoint(w, focused), w.lon, w.lat);
      shown++;
    }

    const t = this.table;
    if (!t || t.length === 0) return;
    const n = t.length;

    if (o.distance) {
      const center = this.map.getCenter();
      const metresPerPx = (40075016.686 * Math.cos((center.lat * Math.PI) / 180)) / (512 * 2 ** this.map.getZoom());
      const interval = STEPS_M.find((s) => s >= SPACING_PX * metresPerPx) ?? STEPS_M[STEPS_M.length - 1];
      const total = t.dist[n - 1];
      let count = 0;
      for (let d = interval; d < total && count < MAX_DISTANCE_MARKERS; d += interval) {
        const i = indexAtDistance(t.dist, d);
        const j = Math.min(i + 1, n - 1);
        const span = t.dist[j] - t.dist[i];
        const f = span > 0 ? (d - t.dist[i]) / span : 0;
        const lon = t.lon[i] + (t.lon[j] - t.lon[i]) * f;
        const lat = t.lat[i] + (t.lat[j] - t.lat[i]) * f;
        if (!visible(lon, lat)) continue;
        // The marker's own value is the exact interval distance, not the nearest point's.
        const text = label(t, i, o, d);
        this.add(this.arrow(t.bearing[i], text, "distance"), lon, lat);
        count++;
      }
    }

    let labels = 0;
    for (const i of this.labelPicks) {
      if (labels >= MAX_SELECTION_LABELS) break;
      if (i >= n || !visible(t.lon[i], t.lat[i])) continue;
      const text = label(t, i, o) || `#${i + 1}`;
      this.add(this.arrow(t.bearing[i], text, "selection"), t.lon[i], t.lat[i]);
      labels++;
    }

    if (o.startEnd) {
      const time = (i: number) => (Number.isNaN(t.time[i]) ? "" : ` · ${new Date(t.time[i]).toLocaleString()}`);
      this.add(this.pin("end", `End · ${formatKm(t.dist[n - 1])}${time(n - 1)}`), t.lon[n - 1], t.lat[n - 1]);
      this.add(this.pin("start", `Start${time(0)}`), t.lon[0], t.lat[0]);
    }
  }
}

/** Large selections are labelled only where each selected run starts and ends. */
function runEnds(sorted: number[]): number[] {
  if (sorted.length <= MAX_SELECTION_LABELS) return sorted;
  const ends: number[] = [];
  for (let k = 0; k < sorted.length; k++) {
    const runStart = k === 0 || sorted[k - 1] !== sorted[k] - 1;
    const runEnd = k === sorted.length - 1 || sorted[k + 1] !== sorted[k] + 1;
    if (runStart || runEnd) ends.push(sorted[k]);
  }
  return ends;
}
