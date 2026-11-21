import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  activityOf,
  api,
  displayName,
  formatDistance,
  type PointTable,
  type TrackSummary,
  type WaypointRow,
} from "../api";
import {
  EMPTY_SELECTION,
  bboxOf,
  clickSelection,
  confirmDelete,
  isSecondaryClick,
  isToggleKey,
  rangeSelection,
  type PointSelection,
} from "../pointEditing";
import { PointMenu } from "./PointMenu";

const ROW = 20;
const OVERSCAN = 10;
/** Long waypoint lists start collapsed so the points stay in view. */
const WAYPOINTS_OPEN_MAX = 20;

interface Props {
  track: TrackSummary;
  /** All tracks of the selected track's file, the selected one included. */
  fileTracks: TrackSummary[];
  waypoints: WaypointRow[];
  table: PointTable | null;
  focusedWaypoint: number | null;
  onSelectTrack: (id: number) => void;
  onFocusWaypoint: (id: number | null) => void;
  onHover: (lonLat: [number, number] | null) => void;
  selection: PointSelection;
  onSelectionChange: (s: PointSelection) => void;
  /** Scrolls the tree to a point selected elsewhere (on the map). */
  reveal: { index: number; seq: number } | null;
  /** An edit is being applied; the shown indices are about to change. */
  locked: boolean;
  onBeginEdit: () => void;
  onZoomTo: (bbox: [number, number, number, number]) => void;
  onPanTo: (lonLat: [number, number]) => void;
  onEdited: () => void;
  onError: (message: string) => void;
}

/** One header row, or a run of point / waypoint rows rendered on demand. */
type Block =
  | { kind: "file"; depth: number }
  | { kind: "wpGroup"; depth: number }
  | { kind: "wps"; depth: number; start: number; end: number }
  | { kind: "track"; depth: number; track: TrackSummary }
  | { kind: "seg"; depth: number; seg: number; start: number; end: number }
  | { kind: "points"; depth: number; start: number; end: number };

const rowCount = (b: Block) => (b.kind === "wps" || b.kind === "points" ? b.end - b.start : 1);

type Menu = { x: number; y: number } & ({ target: "points" } | { target: "seg"; seg: number });

function pad(n: number) {
  return String(n).padStart(2, "0");
}

function timeOfDay(ms: number | null) {
  if (ms == null || Number.isNaN(ms)) return "—";
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function fileName(path: string) {
  return path.split("/").pop() ?? path;
}

/**
 * File → tracks → segments → points, plus the file's waypoints. Levels with a single
 * child are left out, and the whole tree is one virtualised list so tracks with hundreds
 * of thousands of points stay responsive.
 */
export function TrackTree(p: Props) {
  const { track, table, waypoints } = p;
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(300);
  const [menu, setMenu] = useState<Menu | null>(null);
  const closeMenu = useCallback(() => setMenu(null), []);
  const box = useRef<HTMLDivElement>(null);
  const n = table?.length ?? 0;

  useEffect(() => {
    setCollapsed(waypoints.length > WAYPOINTS_OPEN_MAX ? new Set(["wp"]) : new Set());
  }, [track.fileId, waypoints.length]);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const selection = p.selection.set;
  const sorted = useMemo(() => [...selection].sort((a, b) => a - b), [selection]);

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

  /** The file's own segments of the selected track as [start, end) ranges. */
  const segments = useMemo(() => {
    if (!table) return [] as [number, number][];
    const starts: number[] = [];
    for (let i = 0; i < table.length; i++) if (table.segStart[i] === 1 || i === 0) starts.push(i);
    return starts.map((s, k) => [s, starts[k + 1] ?? table.length] as [number, number]);
  }, [table]);

  const blocks = useMemo(() => {
    const out: Block[] = [];
    const showFile = p.fileTracks.length > 1 || waypoints.length > 0;
    const showTracks = p.fileTracks.length > 1;
    let depth = 0;
    if (showFile) {
      out.push({ kind: "file", depth: 0 });
      depth = 1;
      if (waypoints.length) {
        out.push({ kind: "wpGroup", depth });
        if (!collapsed.has("wp")) out.push({ kind: "wps", depth: depth + 1, start: 0, end: waypoints.length });
      }
    }
    const trackList = showTracks ? p.fileTracks : [track];
    for (const t of trackList) {
      if (showTracks) out.push({ kind: "track", depth, track: t });
      if (t.id !== track.id || !table) continue;
      const inner = showTracks ? depth + 1 : depth;
      if (segments.length > 1) {
        segments.forEach(([s, e], k) => {
          out.push({ kind: "seg", depth: inner, seg: k, start: s, end: e });
          if (!collapsed.has(`seg:${s}`)) out.push({ kind: "points", depth: inner + 1, start: s, end: e });
        });
      } else {
        out.push({ kind: "points", depth: inner, start: 0, end: n });
      }
    }
    return out;
  }, [p.fileTracks, waypoints, collapsed, track, table, segments, n]);

  const offsets = useMemo(() => {
    const o = [0];
    for (const b of blocks) o.push(o[o.length - 1] + rowCount(b));
    return o;
  }, [blocks]);
  const totalRows = offsets[offsets.length - 1];

  const toggle = (key: string) => {
    const next = new Set(collapsed);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    setCollapsed(next);
  };

  const select = (i: number, e: { shiftKey: boolean; metaKey: boolean; ctrlKey: boolean }) =>
    p.onSelectionChange(clickSelection(p.selection, i, { shift: e.shiftKey, toggle: isToggleKey(e) }));

  const selectRange = (start: number, end: number) => p.onSelectionChange(rangeSelection(start, end));

  const zoomTo = (indices: Iterable<number>) => {
    setMenu(null);
    const b = table && bboxOf(table, indices);
    if (b) p.onZoomTo(b);
  };

  const run = async (action: () => Promise<unknown>) => {
    setMenu(null);
    if (p.locked) return;
    p.onBeginEdit();
    try {
      await action();
      p.onEdited();
    } catch (e) {
      p.onError(String(e));
    }
  };

  const deletePoints = async (indices: number[], what: string) => {
    setMenu(null);
    if (p.locked || !(await confirmDelete(indices.length, what))) return;
    await run(() => api.deleteTrackPoints(track.id, indices));
  };

  const range = (s: number, e: number) => Array.from({ length: e - s }, (_, k) => s + k);

  // Bring a point selected on the map into view, expanding its segment if needed.
  const revealed = useRef(0);
  useEffect(() => {
    const r = p.reveal;
    const el = box.current;
    if (!r || r.seq === revealed.current || !el) return;
    const seg = segments.find(([s, e]) => r.index >= s && r.index < e);
    if (segments.length > 1 && seg && collapsed.has(`seg:${seg[0]}`)) {
      toggle(`seg:${seg[0]}`);
      return;
    }
    revealed.current = r.seq;
    const bi = blocks.findIndex((b) => b.kind === "points" && r.index >= b.start && r.index < b.end);
    if (bi < 0) return;
    const b = blocks[bi] as { start: number };
    const top = (offsets[bi] + r.index - b.start) * ROW;
    if (top < el.scrollTop || top + ROW > el.scrollTop + el.clientHeight) el.scrollTop = top - el.clientHeight / 2;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [p.reveal, blocks]);

  const pointRow = (i: number, depth: number, top: number) => {
    const t = table!;
    const gap = t.segStart[i] === 2;
    return (
      <div
        key={`p${i}`}
        className={`tree-row point-row${selection.has(i) ? " selected" : ""}${gap ? " gap-start" : ""}`}
        style={{ top, paddingLeft: 8 + depth * 14 }}
        title={gap ? "Line break: GPS glitch or recording gap" : undefined}
        onMouseDown={(e) => {
          if (isSecondaryClick(e)) return;
          box.current?.focus({ preventScroll: true });
          select(i, e);
          p.onPanTo([t.lon[i], t.lat[i]]);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          if (!selection.has(i)) p.onSelectionChange({ set: new Set([i]), anchor: i });
          if (!p.locked) setMenu({ x: e.clientX, y: e.clientY, target: "points" });
        }}
        onMouseEnter={() => p.onHover([t.lon[i], t.lat[i]])}
      >
        <span className="num">{i + 1}</span>
        <span>{timeOfDay(t.time[i])}</span>
        <span className="num">{Number.isNaN(t.ele[i]) ? "—" : Math.round(t.ele[i])}</span>
        <span className="coord">
          {t.lat[i].toFixed(5)}, {t.lon[i].toFixed(5)}
        </span>
      </div>
    );
  };

  const header = (
    key: string,
    depth: number,
    top: number,
    content: React.ReactNode,
    extra: React.HTMLAttributes<HTMLDivElement> & { className?: string } = {},
  ) => (
    <div
      key={key}
      {...extra}
      className={`tree-row tree-header ${extra.className ?? ""}`}
      style={{ top, paddingLeft: 8 + depth * 14 }}
    >
      {content}
    </div>
  );

  const twisty = (key: string) => (
    <button
      className="twisty"
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => {
        e.stopPropagation();
        toggle(key);
      }}
    >
      {collapsed.has(key) ? "▸" : "▾"}
    </button>
  );

  const renderBlockRow = (b: Block, r: number, top: number) => {
    switch (b.kind) {
      case "file":
        return header(
          "file",
          b.depth,
          top,
          <>
            <span className="tree-icon">📄</span>
            <span className="tree-title" title={track.filePath}>
              {fileName(track.filePath)}
            </span>
            <span className="muted">
              {p.fileTracks.length} {p.fileTracks.length === 1 ? "track" : "tracks"}
              {waypoints.length ? ` · ${waypoints.length} waypoints` : ""}
            </span>
          </>,
        );
      case "wpGroup":
        return header(
          "wpGroup",
          b.depth,
          top,
          <>
            {twisty("wp")}
            <span className="tree-icon">🚩</span>
            <span className="tree-title">Waypoints</span>
            <span className="muted">{waypoints.length}</span>
          </>,
          { onClick: () => toggle("wp") },
        );
      case "wps": {
        const w = waypoints[b.start + r];
        const focused = p.focusedWaypoint === w.id;
        return header(
          `w${w.id}`,
          b.depth,
          top,
          <>
            <span className="tree-icon">{w.kind === "lap" ? "⏱" : "📍"}</span>
            <span className="tree-title" title={w.description ?? undefined}>
              {w.name ?? "Waypoint"}
            </span>
            <span className="muted">
              {w.time != null ? timeOfDay(w.time) : ""}
              {w.ele != null ? ` · ${Math.round(w.ele)} m` : ""}
            </span>
          </>,
          {
            className: `waypoint-row${focused ? " selected" : ""}`,
            onMouseDown: () => {
              p.onFocusWaypoint(focused ? null : w.id);
              if (!focused) p.onPanTo([w.lon, w.lat]);
            },
            onMouseEnter: () => p.onHover([w.lon, w.lat]),
          },
        );
      }
      case "track": {
        const t = b.track;
        const current = t.id === track.id;
        const icon = activityOf(t)?.icon ?? "〰";
        return header(
          `t${t.id}`,
          b.depth,
          top,
          <>
            <span className="twisty">{current ? "▾" : "▸"}</span>
            <span className="tree-icon">{icon}</span>
            <span className="tree-title">{displayName(t)}</span>
            <span className="muted">
              {formatDistance(t.distanceM)} · {t.pointCount.toLocaleString()} pts
            </span>
          </>,
          {
            className: `track-row${current ? " current" : ""}`,
            onMouseDown: () => !current && p.onSelectTrack(t.id),
          },
        );
      }
      case "seg": {
        const t = table!;
        const key = `seg:${b.start}`;
        const dist = t.dist[b.end - 1] - t.dist[b.start];
        const segSelected = sorted.length === b.end - b.start && sorted[0] === b.start;
        return header(
          key,
          b.depth,
          top,
          <>
            {twisty(key)}
            <span className="tree-title">Segment {b.seg + 1}</span>
            <span className="muted">
              {formatDistance(dist)} · {(b.end - b.start).toLocaleString()} pts · {timeOfDay(t.time[b.start])}–
              {timeOfDay(t.time[b.end - 1])}
            </span>
          </>,
          {
            className: `seg-row${segSelected ? " selected" : ""}`,
            onMouseDown: (e) => {
              if (isSecondaryClick(e)) return;
              box.current?.focus({ preventScroll: true });
              selectRange(b.start, b.end);
              zoomTo(range(b.start, b.end));
            },
            onContextMenu: (e) => {
              e.preventDefault();
              selectRange(b.start, b.end);
              if (!p.locked) setMenu({ x: e.clientX, y: e.clientY, target: "seg", seg: b.seg });
            },
          },
        );
      }
      case "points":
        return pointRow(b.start + r, b.depth, top);
    }
  };

  // Render only the rows in view: find the first visible block by binary search.
  const firstRow = Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN);
  const lastRow = Math.min(totalRows, Math.ceil((scrollTop + height) / ROW) + OVERSCAN);
  const rows: React.ReactNode[] = [];
  let lo = 0;
  let hi = blocks.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (offsets[mid] <= firstRow) lo = mid;
    else hi = mid - 1;
  }
  for (let bi = lo; bi < blocks.length && offsets[bi] < lastRow; bi++) {
    const b = blocks[bi];
    const from = Math.max(firstRow, offsets[bi]);
    const to = Math.min(lastRow, offsets[bi + 1]);
    for (let row = from; row < to; row++) rows.push(renderBlockRow(b, row - offsets[bi], row * ROW));
  }

  const menuSeg = menu?.target === "seg" ? segments[menu.seg] : null;

  return (
    <div className="points">
      <div className="points-header">
        <span>Contents</span>
        <span className="muted">
          {sorted.length
            ? `${sorted.length.toLocaleString()} of ${n.toLocaleString()} points selected`
            : `${n.toLocaleString()} points`}
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
        onMouseLeave={() => p.onHover(null)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "a") {
            e.preventDefault();
            selectRange(0, n);
          } else if (e.key === "Backspace" || e.key === "Delete") {
            e.preventDefault();
            deletePoints(sorted, `${sorted.length.toLocaleString()} points`);
          } else if (e.key === "Escape") {
            p.onSelectionChange(EMPTY_SELECTION);
          }
        }}
      >
        {!table && <div className="hint">Loading points…</div>}
        <div style={{ height: totalRows * ROW, position: "relative" }}>{rows}</div>
      </div>
      {menu?.target === "points" && table && (
        <PointMenu
          x={menu.x}
          y={menu.y}
          trackId={track.id}
          table={table}
          selection={sorted}
          onClose={closeMenu}
          onZoomTo={p.onZoomTo}
          onBeginEdit={p.onBeginEdit}
          onEdited={p.onEdited}
          onError={p.onError}
        />
      )}
      {menu?.target === "seg" && (
        <div className="context-menu" style={{ left: menu.x, top: menu.y }} onMouseDown={(e) => e.stopPropagation()}>
          {menuSeg && menu.target === "seg" && (
            <>
              <button
                onClick={() => run(() => api.extractTrackRange(track.id, menuSeg[0], menuSeg[1]))}
                title="The segment becomes a track of its own; this track keeps the other segments"
              >
                Make separate track
              </button>
              <button
                disabled={menu.seg + 1 >= segments.length}
                onClick={() => run(() => api.joinTrackSegment(track.id, segments[menu.seg + 1][0]))}
              >
                Join with next segment
              </button>
              <button onClick={() => deletePoints(range(menuSeg[0], menuSeg[1]), `segment ${menu.seg + 1}`)}>
                Delete segment
              </button>
              <hr />
              <button onClick={() => zoomTo(range(menuSeg[0], menuSeg[1]))}>Zoom to segment</button>
            </>
          )}
        </div>
      )}
    </div>
  );
}
