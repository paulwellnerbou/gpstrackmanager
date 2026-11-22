import type { ExpressionSpecification } from "maplibre-gl";
import type { Feature } from "geojson";
import type { PointTable } from "../api";

export type ColorBy = "none" | "speed" | "altitude";

export const COLOR_BY: { value: ColorBy; label: string }[] = [
  { value: "none", label: "Plain" },
  { value: "speed", label: "Speed" },
  { value: "altitude", label: "Altitude" },
];

export interface ColorScale {
  by: Exclude<ColorBy, "none">;
  min: number;
  max: number;
  /** Values lie beyond `min` / `max` and are drawn in the end colours. */
  lowOpen: boolean;
  highOpen: boolean;
}

/**
 * One-hue sequential ramp, low to high. Neither end may vanish into the map, so the light
 * map starts at a mid blue; on a dark map the anchor flips and low values are the darker.
 */
const RAMPS = {
  light: ["#86b6ef", "#6da7ec", "#5598e7", "#3987e5", "#2a78d6", "#256abf", "#1c5cab", "#184f95", "#104281", "#0d366b"],
  dark: ["#184f95", "#1c5cab", "#256abf", "#2a78d6", "#3987e5", "#5598e7", "#6da7ec", "#86b6ef", "#9ec5f4", "#b7d3f6"],
};
/** Stretches without a value, e.g. speed on a track without timestamps. */
const UNKNOWN = { light: "#a8a49c", dark: "#6f6b65" };
const BINS = RAMPS.light.length;

/**
 * The scale spans the whole track, not the view, so colours stay put while panning.
 * Percentiles rather than min/max: a single GPS spike would squeeze everything else into
 * one colour.
 */
export function colorScale(t: PointTable, by: ColorScale["by"]): ColorScale | null {
  const src = by === "speed" ? t.speed : t.ele;
  // A strided sample keeps the sort cheap on tracks with a million points.
  const step = Math.max(1, Math.floor(t.length / 50_000));
  const vals: number[] = [];
  for (let i = 0; i < t.length; i += step) if (Number.isFinite(src[i])) vals.push(src[i]);
  if (!vals.length) return null;
  vals.sort((a, b) => a - b);
  const [lo, hi] = by === "speed" ? [0.02, 0.98] : [0.01, 0.99];
  const min = vals[Math.floor(lo * (vals.length - 1))];
  let max = vals[Math.ceil(hi * (vals.length - 1))];
  if (max - min < 1e-6) max = min + 1;
  let lowest = Infinity;
  let highest = -Infinity;
  for (const v of src) {
    if (v < lowest) lowest = v;
    if (v > highest) highest = v;
  }
  return { by, min, max, lowOpen: lowest < min, highOpen: highest > max };
}

/** Value of the drawn edge from point `a` to point `b`, which may skip thinned-out points. */
function edgeValue(t: PointTable, by: ColorScale["by"], a: number, b: number): number {
  if (by === "altitude") {
    const [x, y] = [t.ele[a], t.ele[b]];
    return Number.isNaN(x) ? y : Number.isNaN(y) ? x : (x + y) / 2;
  }
  // Over a long thinned edge the average speed is exact; between neighbours the smoothed
  // per-point speeds jitter less than distance over a one-second step.
  if (b - a > 2) {
    const dt = (t.time[b] - t.time[a]) / 1000;
    if (dt > 0) return (t.dist[b] - t.dist[a]) / dt;
  }
  let sum = 0;
  let n = 0;
  for (let k = a; k <= b; k++) {
    if (!Number.isNaN(t.speed[k])) {
      sum += t.speed[k];
      n++;
    }
  }
  return n ? sum / n : NaN;
}

function binOf(v: number, s: ColorScale): number {
  if (Number.isNaN(v)) return -1;
  return Math.max(0, Math.min(BINS - 1, Math.floor(((v - s.min) / (s.max - s.min)) * BINS)));
}

/**
 * Lines through the given point indices (NaN between lines), cut wherever the colour bin
 * changes so each feature has one colour. Runs share their end vertex, so the line stays
 * continuous.
 */
export function coloredLines(idx: Float64Array, t: PointTable, s: ColorScale): Feature[] {
  const out: Feature[] = [];
  let coords: [number, number][] = [];
  let bin = NaN;
  let prev = -1;
  const flush = () => {
    if (coords.length > 1) {
      out.push({ type: "Feature", properties: { b: bin }, geometry: { type: "LineString", coordinates: coords } });
    }
  };
  for (const x of idx) {
    // Indices past the table belong to a newer version of the track than the table.
    if (Number.isNaN(x) || x >= t.length) {
      flush();
      coords = [];
      prev = -1;
      continue;
    }
    const p: [number, number] = [t.lon[x], t.lat[x]];
    if (prev >= 0) {
      const b = binOf(edgeValue(t, s.by, prev, x), s);
      if (b !== bin) {
        const last = coords[coords.length - 1];
        flush();
        coords = [last];
        bin = b;
      }
    }
    coords.push(p);
    prev = x;
  }
  flush();
  return out;
}

/** Line colour by bin; features without a bin (the plain line) take `plain`. */
export function lineColor(plain: string, brightness: "light" | "dark"): ExpressionSpecification {
  const stops = RAMPS[brightness].flatMap((c, i) => [i, c]);
  return ["match", ["coalesce", ["get", "b"], -2], -1, UNKNOWN[brightness], ...stops, plain] as ExpressionSpecification;
}

/** Hard-stop gradient for the legend, matching the drawn bins. */
export function legendGradient(brightness: "light" | "dark"): string {
  const ramp = RAMPS[brightness];
  const stops = ramp.map((c, i) => `${c} ${(i / BINS) * 100}% ${((i + 1) / BINS) * 100}%`);
  return `linear-gradient(to right, ${stops.join(", ")})`;
}

function formatValue(s: ColorScale, v: number): string {
  return s.by === "speed" ? `${Math.round(v * 3.6)}` : `${Math.round(v).toLocaleString()}`;
}

const SCALE_UNIT: Record<ColorScale["by"], string> = { speed: "km/h", altitude: "m" };

/** Legend end labels; open ends say that values beyond them share the end colour. */
export function scaleLabels(s: ColorScale): [string, string] {
  return [
    `${s.lowOpen ? "≤ " : ""}${formatValue(s, s.min)}`,
    `${s.highOpen ? "≥ " : ""}${formatValue(s, s.max)} ${SCALE_UNIT[s.by]}`,
  ];
}

/** Properties for a line that should be coloured but has no values yet, or none at all. */
export const UNCOLORED = { b: -1 };
