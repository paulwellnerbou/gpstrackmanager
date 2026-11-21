import { useEffect, useMemo, useRef, useState } from "react";
import { ask } from "@tauri-apps/plugin-dialog";
import { api, type PointTable, type TrackSummary } from "../api";

const ROW = 20;
const OVERSCAN = 10;

interface Props {
  track: TrackSummary;
  table: PointTable | null;
  onHover: (lonLat: [number, number] | null) => void;
  onSelectionChange: (sortedIndices: number[]) => void;
  onZoomTo: (bbox: [number, number, number, number]) => void;
  onEdited: () => void;
  onError: (message: string) => void;
}

function pad(n: number) {
  return String(n).padStart(2, "0");
}

function timeOfDay(ms: number) {
  if (Number.isNaN(ms)) return "—";
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** Virtualised so tracks with hundreds of thousands of points stay responsive. */
export function PointList({ track, table, onHover, onSelectionChange, onZoomTo, onEdited, onError }: Props) {
  const [selection, setSelection] = useState<Set<number>>(new Set());
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(300);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const anchor = useRef<number | null>(null);
  const box = useRef<HTMLDivElement>(null);

  // Indices refer to one version of the track; a new table invalidates them.
  useEffect(() => {
    setSelection(new Set());
    anchor.current = null;
  }, [table]);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const sorted = useMemo(() => [...selection].sort((a, b) => a - b), [selection]);

  useEffect(() => onSelectionChange(sorted), [sorted, onSelectionChange]);
  useEffect(() => () => onSelectionChange([]), [onSelectionChange]);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  const n = table?.length ?? 0;

  const select = (i: number, e: { shiftKey: boolean; metaKey: boolean; ctrlKey: boolean }) => {
    if (e.shiftKey && anchor.current != null) {
      const [a, b] = [Math.min(anchor.current, i), Math.max(anchor.current, i)];
      const next = e.metaKey ? new Set(selection) : new Set<number>();
      for (let k = a; k <= b; k++) next.add(k);
      setSelection(next);
      return;
    }
    if (e.metaKey || e.ctrlKey) {
      const next = new Set(selection);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      setSelection(next);
    } else {
      setSelection(new Set([i]));
    }
    anchor.current = i;
  };

  const run = async (action: () => Promise<unknown>) => {
    setMenu(null);
    try {
      await action();
      onEdited();
    } catch (e) {
      onError(String(e));
    }
  };

  const deleteSelected = async () => {
    setMenu(null);
    if (!sorted.length) return;
    if (sorted.length > 1) {
      const ok = await ask(`Delete ${sorted.length.toLocaleString()} points from this track?`, {
        title: "Delete points",
        kind: "warning",
        okLabel: "Delete",
      });
      if (!ok) return;
    }
    await run(() => api.deleteTrackPoints(track.id, sorted));
  };

  const splitAt = sorted[0];
  const canSplit = sorted.length > 0 && splitAt > 0 && splitAt < n;

  const zoomToSelection = () => {
    setMenu(null);
    if (!table || !sorted.length) return;
    let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
    for (const i of sorted) {
      x0 = Math.min(x0, table.lon[i]);
      x1 = Math.max(x1, table.lon[i]);
      y0 = Math.min(y0, table.lat[i]);
      y1 = Math.max(y1, table.lat[i]);
    }
    onZoomTo([x0, y0, x1, y1]);
  };

  const first = Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN);
  const last = Math.min(n, Math.ceil((scrollTop + height) / ROW) + OVERSCAN);
  const rows = [];
  for (let i = first; i < last && table; i++) {
    const t = table.time[i];
    rows.push(
      <div
        key={i}
        className={`point-row${selection.has(i) ? " selected" : ""}${table.segStart[i] && i > 0 ? " seg-start" : ""}`}
        style={{ top: i * ROW }}
        title={Number.isNaN(t) ? undefined : new Date(t).toLocaleString()}
        onMouseDown={(e) => {
          if (e.button !== 0) return;
          box.current?.focus({ preventScroll: true });
          select(i, e);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          if (!selection.has(i)) {
            setSelection(new Set([i]));
            anchor.current = i;
          }
          setMenu({ x: e.clientX, y: e.clientY });
        }}
        onMouseEnter={() => onHover([table.lon[i], table.lat[i]])}
      >
        <span className="num">{i + 1}</span>
        <span>{timeOfDay(t)}</span>
        <span className="num">{Number.isNaN(table.ele[i]) ? "—" : Math.round(table.ele[i])}</span>
        <span className="coord">
          {table.lat[i].toFixed(5)}, {table.lon[i].toFixed(5)}
        </span>
      </div>,
    );
  }

  return (
    <div className="points">
      <div className="points-header">
        <span>Points</span>
        <span className="muted">
          {sorted.length ? `${sorted.length.toLocaleString()} of ${n.toLocaleString()} selected` : n.toLocaleString()}
        </span>
      </div>
      <div className="point-cols">
        <span className="num">#</span>
        <span>Time</span>
        <span className="num">Ele</span>
        <span>Lat, Lon</span>
      </div>
      <div
        ref={box}
        className="point-list"
        tabIndex={0}
        onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
        onMouseLeave={() => onHover(null)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "a") {
            e.preventDefault();
            setSelection(new Set(Array.from({ length: n }, (_, k) => k)));
          } else if (e.key === "Backspace" || e.key === "Delete") {
            e.preventDefault();
            deleteSelected();
          } else if (e.key === "Escape") {
            setSelection(new Set());
          }
        }}
      >
        {!table && <div className="hint">Loading points…</div>}
        <div style={{ height: n * ROW, position: "relative" }}>{rows}</div>
      </div>
      {menu && (
        <div className="context-menu" style={{ left: menu.x, top: menu.y }} onMouseDown={(e) => e.stopPropagation()}>
          <button disabled={!canSplit} onClick={() => run(() => api.splitTrack(track.id, splitAt))}>
            Split track here (new track starts at #{(splitAt ?? 0) + 1})
          </button>
          <button disabled={!sorted.length} onClick={deleteSelected}>
            Delete {sorted.length === 1 ? "point" : `${sorted.length.toLocaleString()} points`}
          </button>
          <hr />
          <button disabled={!sorted.length} onClick={zoomToSelection}>
            Zoom to selection
          </button>
        </div>
      )}
    </div>
  );
}
