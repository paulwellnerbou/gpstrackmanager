import { Marker, type Map as MapLibreMap } from "maplibre-gl";
import type { PointTable, WaypointRow } from "../api";
import type { ColorBy } from "./trackColoring";
import { formatClock, spansDays } from "../timeFormat";

export interface MarkerOptions {
  /** How the selected track is drawn: as a line, as its points, or both. */
  pointView: "line" | "points" | "both";
  /** Colours the selected track's line by a value along it. */
  colorBy: ColorBy;
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
  colorBy: "none",
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

/** Drawn twice, wide in the line colour and narrow on top, so it shows on the line and off it. */
const CHEVRON_SVG =
  '<svg viewBox="0 0 18 18" width="18" height="18"><path class="trk-chevron-edge" d="M5 11.5 9 7.5l4 4"/><path class="trk-chevron-face" d="M5 11.5 9 7.5l4 4"/></svg>';

/** The tip, at the bottom centre, sits exactly on the waypoint. */
const PIN_SVG =
  '<svg viewBox="0 0 18 24" width="18" height="24"><path class="trk-wp-body" d="M9 24 3.2 13A7 7 0 1 1 14.8 13Z"/><circle class="trk-wp-eye" cx="9" cy="9" r="2.5"/></svg>';

/** Flags stand on the point at the foot of their pole; the finish flag flies the other way,
    so on a round trip both stay visible. */
const START_FLAG_SVG =
  '<svg viewBox="0 0 28 34" width="28" height="34"><path class="trk-pole" d="M5 30V3"/><path class="trk-flag-start" d="M6 3h17l-4 6 4 6H6z"/><circle class="trk-foot" cx="5" cy="30" r="3.5"/></svg>';
const FINISH_FLAG_SVG =
  '<svg viewBox="0 0 28 34" width="28" height="34"><path class="trk-pole" d="M23 30V3"/><rect class="trk-flag-finish" x="4" y="3" width="18" height="12"/><path class="trk-flag-check" d="M4 3h6v6H4zM16 3h6v6h-6zM10 9h6v6h-6z"/><circle class="trk-foot" cx="23" cy="30" r="3.5"/></svg>';

function formatKm(m: number) {
  const km = m / 1000;
  return `${km.toLocaleString(undefined, { maximumFractionDigits: km < 10 ? 1 : 0 })} km`;
}

function label(t: PointTable, i: number, o: MarkerOptions, distanceOverride?: number): string {
  const parts: string[] = [];
  if (o.showDistance) parts.push(formatKm(distanceOverride ?? t.dist[i]));
  if (o.showSpeed && !Number.isNaN(t.speed[i])) parts.push(`${Math.round(t.speed[i] * 3.6)} km/h`);
  if (o.showAltitude && !Number.isNaN(t.ele[i])) parts.push(`${Math.round(t.ele[i])} m`);
  if (o.showTime && !Number.isNaN(t.time[i])) parts.push(formatClock(t.time[i], spansDays(t.time), false));
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
  /** Kept across renders so a focus change restyles the element and its CSS transition runs. */
  private waypointMarkers = new Map<number, Marker>();
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

  private add(
    el: HTMLElement,
    lon: number,
    lat: number,
    anchor: "center" | "bottom-left" | "bottom-right" = "center",
    offset: [number, number] = [0, 0],
  ) {
    this.markers.push(new Marker({ element: el, anchor, offset }).setLngLat([lon, lat]).addTo(this.map));
  }

  private flag(kind: "start" | "end", text: string, lon: number, lat: number) {
    const el = document.createElement("div");
    el.className = `trk-pin trk-pin-${kind}`;
    el.title = text;
    el.setAttribute("aria-label", text);
    el.innerHTML = kind === "start" ? START_FLAG_SVG : FINISH_FLAG_SVG;
    // Offsets put the pole's foot (5px in from the side, 4px up) on the point.
    if (kind === "start") this.add(el, lon, lat, "bottom-left", [-5, 4]);
    else this.add(el, lon, lat, "bottom-right", [5, 4]);
  }

  private arrow(bearing: number, text: string, kind: "distance" | "selection") {
    const el = document.createElement("div");
    el.className = `trk-mk trk-mk-${kind}`;
    if (!Number.isNaN(bearing)) {
      const a = document.createElement("div");
      a.className = "trk-chevron";
      a.style.transform = `rotate(${bearing - this.map.getBearing()}deg)`;
      a.innerHTML = CHEVRON_SVG;
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
    let m = this.waypointMarkers.get(w.id);
    if (!m) {
      const div = document.createElement("div");
      div.className = `trk-wp trk-wp-${w.kind}`;
      if (w.kind === "waypoint") div.innerHTML = PIN_SVG;
      const anchor = w.kind === "waypoint" ? "bottom" : "center";
      m = new Marker({ element: div, anchor }).setLngLat([w.lon, w.lat]).addTo(this.map);
      this.waypointMarkers.set(w.id, m);
    } else {
      m.setLngLat([w.lon, w.lat]);
    }
    const el = m.getElement();
    // classList, not className: MapLibre's own positioning classes live on this element.
    el.classList.toggle("focused", focused);
    const when = w.time != null ? ` · ${new Date(w.time).toLocaleString()}` : "";
    el.title = `${w.name ?? "Waypoint"}${when}${w.description ? `\n${w.description}` : ""}`;
    // The pin stays, so its focus transition can run; only the label is rebuilt.
    el.querySelector(".trk-label")?.remove();
    if (focused) {
      const l = document.createElement("div");
      l.className = "trk-label";
      l.textContent = w.name ?? "Waypoint";
      el.appendChild(l);
    }
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
    const keep = new Set<number>();
    for (const w of this.waypoints) {
      const focused = w.id === this.focusedWaypoint;
      if (!focused && (!o.waypoints || shown >= MAX_WAYPOINTS)) continue;
      if (!visible(w.lon, w.lat)) continue;
      this.waypoint(w, focused);
      keep.add(w.id);
      shown++;
    }
    for (const [id, m] of this.waypointMarkers) {
      if (keep.has(id)) continue;
      m.remove();
      this.waypointMarkers.delete(id);
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
      this.flag("end", `Finish · ${formatKm(t.dist[n - 1])}${time(n - 1)}`, t.lon[n - 1], t.lat[n - 1]);
      this.flag("start", `Start${time(0)}`, t.lon[0], t.lat[0]);
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
