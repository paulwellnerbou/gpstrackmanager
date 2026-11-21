import { useEffect, useRef, useState } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { api, type TrackSummary } from "../api";
import { useColorScheme } from "../useColorScheme";

interface Props {
  track: TrackSummary | null;
  version: number;
  onCursor: (lonLat: [number, number] | null) => void;
}

/** Elevation over distance; hovering moves a marker on the map. */
export function Profile({ track, version, onCursor }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const plot = useRef<uPlot | null>(null);
  const [empty, setEmpty] = useState(false);
  const scheme = useColorScheme();
  const onCursorRef = useRef(onCursor);
  onCursorRef.current = onCursor;

  useEffect(() => {
    let cancelled = false;
    plot.current?.destroy();
    plot.current = null;
    if (!track || !box.current) return;
    const el = box.current;
    const width = Math.max(300, el.clientWidth);
    api.trackProfile(track.id, Math.min(4000, width * 2)).then((p) => {
      if (cancelled) return;
      setEmpty(p.dist.length === 0);
      if (p.dist.length === 0) return;
      const km = Array.from(p.dist, (d) => d / 1000);
      const axisColor = scheme === "dark" ? "#9a958d" : "#666";
      const gridColor = scheme === "dark" ? "rgba(255,255,255,0.08)" : "rgba(0,0,0,0.07)";
      plot.current = new uPlot(
        {
          width,
          height: el.clientHeight - 8,
          legend: { show: false },
          // Without spread (no distance covered, flat elevation) uPlot falls back to 0–100,
          // which reads as a 100 km track; keep the axis around the actual value instead.
          scales: {
            x: { time: false, range: (_u, min, max) => (max > min ? [min, max] : [min, min + 0.1]) },
            y: { range: (_u, min, max) => (max > min ? uPlot.rangeNum(min, max, 0.1, true) : [min - 10, max + 10]) },
          },
          axes: [
            {
              values: (_u, ticks) => ticks.map((v) => `${v} km`),
              stroke: axisColor,
              grid: { stroke: gridColor },
              ticks: { stroke: gridColor },
            },
            {
              values: (_u, ticks) => ticks.map((v) => `${v} m`),
              stroke: axisColor,
              grid: { stroke: gridColor },
              ticks: { stroke: gridColor },
              size: 60,
            },
          ],
          series: [{}, { stroke: "#d6336c", fill: "rgba(214,51,108,0.12)", width: 1.5, points: { show: false } }],
          hooks: {
            setCursor: [
              (u) => {
                const i = u.cursor.idx;
                onCursorRef.current(i == null ? null : [p.lon[i], p.lat[i]]);
              },
            ],
          },
        },
        [km, Array.from(p.ele)],
        el,
      );
    });
    const ro = new ResizeObserver(() => plot.current?.setSize({ width: el.clientWidth, height: el.clientHeight - 8 }));
    ro.observe(el);
    return () => {
      cancelled = true;
      ro.disconnect();
      plot.current?.destroy();
      plot.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [track?.id, version, scheme]);

  return (
    <div className="profile" onMouseLeave={() => onCursor(null)}>
      {!track && <div className="hint">Select a track to see its elevation profile.</div>}
      {track && empty && <div className="hint">This track has no elevation data.</div>}
      <div ref={box} className="profile-plot" />
    </div>
  );
}
