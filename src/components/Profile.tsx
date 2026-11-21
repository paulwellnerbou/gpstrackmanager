import { useEffect, useRef, useState } from "react";
import { usePresence } from "../presence";
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
          scales: { x: { time: false } },
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

  const noTrack = usePresence(!track);
  const noEle = usePresence(!!track && empty);

  return (
    <div className="profile" onMouseLeave={() => onCursor(null)}>
      {noTrack.shown && (
        <div className={noTrack.closing ? "hint closing" : "hint"}>Select a track to see its elevation profile.</div>
      )}
      {noEle.shown && <div className={noEle.closing ? "hint closing" : "hint"}>This track has no elevation data.</div>}
      <div ref={box} className="profile-plot" />
    </div>
  );
}
