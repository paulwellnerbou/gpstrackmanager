import type { GeoJSONSource, Map as MapLibreMap, PointLike } from "maplibre-gl";
import type { PointTable } from "../api";

export type PointView = "line" | "points" | "both";

/** Above this many points in view, dots are not drawn: they would be an unreadable blob. */
export const MAX_DOTS = 5000;

const EMPTY = { type: "FeatureCollection" as const, features: [] };

/**
 * Draws the selected track's points in the current viewport as dots. Returns the number
 * of points in view when there are too many to draw, otherwise null.
 */
export function updateDots(map: MapLibreMap, table: PointTable | null, view: PointView): number | null {
  const src = map.getSource("dots") as GeoJSONSource | undefined;
  if (!src) return null;
  if (!table || view === "line") {
    src.setData(EMPTY);
    return null;
  }
  const b = map.getBounds();
  const padX = (b.getEast() - b.getWest()) * 0.05;
  const padY = (b.getNorth() - b.getSouth()) * 0.05;
  const [x0, x1, y0, y1] = [b.getWest() - padX, b.getEast() + padX, b.getSouth() - padY, b.getNorth() + padY];
  const inView: number[] = [];
  let count = 0;
  for (let i = 0; i < table.length; i++) {
    const lon = table.lon[i];
    const lat = table.lat[i];
    if (lon < x0 || lon > x1 || lat < y0 || lat > y1) continue;
    if (++count <= MAX_DOTS) inView.push(i);
  }
  if (count > MAX_DOTS) {
    src.setData(EMPTY);
    return count;
  }
  src.setData({
    type: "FeatureCollection",
    features: inView.map((i) => ({
      type: "Feature",
      id: i,
      properties: {},
      geometry: { type: "Point", coordinates: [table.lon[i], table.lat[i]] },
    })),
  });
  return null;
}

/** The drawn point nearest to a screen position, within a few pixels. */
export function pointAt(map: MapLibreMap, p: { x: number; y: number }, radius = 7): number | null {
  if (!map.getLayer("dots")) return null;
  const box: [PointLike, PointLike] = [
    [p.x - radius, p.y - radius],
    [p.x + radius, p.y + radius],
  ];
  let best: number | null = null;
  let bestD = Infinity;
  for (const f of map.queryRenderedFeatures(box, { layers: ["dots"] })) {
    if (typeof f.id !== "number" || f.geometry.type !== "Point") continue;
    const q = map.project(f.geometry.coordinates as [number, number]);
    const d = (q.x - p.x) ** 2 + (q.y - p.y) ** 2;
    if (d < bestD) {
      bestD = d;
      best = f.id;
    }
  }
  return best;
}

/** All points of the track inside a screen rectangle, drawn as dots or not. */
export function pointsInRect(map: MapLibreMap, table: PointTable, a: PointLike, b: PointLike): number[] {
  const [ax, ay] = Array.isArray(a) ? a : [a.x, a.y];
  const [bx, by] = Array.isArray(b) ? b : [b.x, b.y];
  const [left, right, top, bottom] = [Math.min(ax, bx), Math.max(ax, bx), Math.min(ay, by), Math.max(ay, by)];
  // Coarse geographic prefilter from the corners, exact test in screen space after,
  // so a rotated map still selects exactly what the rectangle covers.
  const corners = [
    map.unproject([left, top]),
    map.unproject([right, top]),
    map.unproject([left, bottom]),
    map.unproject([right, bottom]),
  ];
  const lons = corners.map((c) => c.lng);
  const lats = corners.map((c) => c.lat);
  const [x0, x1, y0, y1] = [Math.min(...lons), Math.max(...lons), Math.min(...lats), Math.max(...lats)];
  const out: number[] = [];
  for (let i = 0; i < table.length; i++) {
    const lon = table.lon[i];
    const lat = table.lat[i];
    if (lon < x0 || lon > x1 || lat < y0 || lat > y1) continue;
    const q = map.project([lon, lat]);
    if (q.x >= left && q.x <= right && q.y >= top && q.y <= bottom) out.push(i);
  }
  return out;
}

/**
 * ⇧-drag draws a selection rectangle instead of MapLibre's box zoom. Short drags are
 * ignored so a ⇧-click still reaches the click handler.
 */
export function installBoxSelect(
  map: MapLibreMap,
  enabled: () => boolean,
  onSelect: (a: PointLike, b: PointLike, additive: boolean) => void,
) {
  map.boxZoom.disable();
  const container = map.getCanvasContainer();
  map.on("mousedown", (e) => {
    const ev = e.originalEvent;
    if (!ev.shiftKey || ev.button !== 0 || !enabled()) return;
    e.preventDefault();
    const origin = container.getBoundingClientRect();
    const start = { x: ev.clientX - origin.left, y: ev.clientY - origin.top };
    const rect = document.createElement("div");
    rect.className = "box-select";
    container.appendChild(rect);
    const draw = (x: number, y: number) => {
      rect.style.left = `${Math.min(start.x, x)}px`;
      rect.style.top = `${Math.min(start.y, y)}px`;
      rect.style.width = `${Math.abs(x - start.x)}px`;
      rect.style.height = `${Math.abs(y - start.y)}px`;
    };
    draw(start.x, start.y);
    const move = (m: MouseEvent) => draw(m.clientX - origin.left, m.clientY - origin.top);
    const up = (u: MouseEvent) => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      rect.remove();
      const end = { x: u.clientX - origin.left, y: u.clientY - origin.top };
      if (Math.abs(end.x - start.x) < 4 && Math.abs(end.y - start.y) < 4) return;
      onSelect([start.x, start.y], [end.x, end.y], u.metaKey || u.ctrlKey);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  });
}
