import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  activityOf,
  api,
  displayName,
  routeName,
  formatDistance,
  type FileProblem,
  type GpsDevice,
  type Root,
  type ScanProgress,
  type TrackSummary,
} from "../api";

interface Props {
  roots: Root[];
  tracks: TrackSummary[];
  selectedId: number | null;
  scan: ScanProgress | null;
  problems: FileProblem[];
  query: string;
  onQuery: (q: string) => void;
  onSelect: (id: number) => void;
  onRootsChanged: () => void;
  onScan: (rootId: number) => void;
  onImported: () => void;
  onError: (message: string) => void;
}

/**
 * Rows of one kind (with or without a route line and tags) share a height, so the list can
 * render only what is visible. Every line is a single line, so the height of a kind does not
 * depend on the width; these are minimums, raised once per kind if its rows need more
 * (larger text).
 */
const ROW_H = 50;
const ROUTE_H = 17;
const TAGS_H = 20;
const OVERSCAN = 8;

const ROW_KINDS = ["", "r", "t", "rt"] as const;
type RowKind = (typeof ROW_KINDS)[number];

/** The route line is shown only when it adds something to the name. */
function routeLine(t: TrackSummary): string | null {
  const route = routeName(t);
  return route && route !== displayName(t) ? route : null;
}

function rowKind(t: TrackSummary): RowKind {
  return `${routeLine(t) ? "r" : ""}${t.tags.length ? "t" : ""}` as RowKind;
}

const DEFAULT_ROW_HEIGHTS: Record<RowKind, number> = {
  "": ROW_H,
  r: ROW_H + ROUTE_H,
  t: ROW_H + TAGS_H,
  rt: ROW_H + ROUTE_H + TAGS_H,
};

export function Sidebar(p: Props) {
  const [devices, setDevices] = useState<GpsDevice[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [deviceMsg, setDeviceMsg] = useState<string | null>(null);
  const [showProblems, setShowProblems] = useState(false);

  const list = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewHeight, setViewHeight] = useState(600);
  useEffect(() => {
    const el = list.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewHeight(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const [rowHeight, setRowHeight] = useState(DEFAULT_ROW_HEIGHTS);
  const measured = useRef(new Set<RowKind>());
  useLayoutEffect(() => {
    const el = list.current;
    if (!el || measured.current.size === ROW_KINDS.length) return;
    let next: Record<RowKind, number> | null = null;
    for (const kind of ROW_KINDS) {
      if (measured.current.has(kind)) continue;
      const row = el.querySelector<HTMLElement>(`.list-row[data-kind="${kind}"]`);
      if (!row) continue;
      measured.current.add(kind);
      // +1 for the bottom border, which scrollHeight leaves out.
      const need = row.scrollHeight + 1;
      if (need > rowHeight[kind]) next = { ...(next ?? rowHeight), [kind]: need };
    }
    if (next) setRowHeight(next);
  });
  const offsets = useMemo(() => {
    const o = new Float64Array(p.tracks.length + 1);
    p.tracks.forEach((t, i) => (o[i + 1] = o[i] + rowHeight[rowKind(t)]));
    return o;
  }, [p.tracks, rowHeight]);
  const rowAt = (y: number) => {
    let [lo, hi] = [0, p.tracks.length];
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (offsets[mid + 1] <= y) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  };
  const first = Math.max(0, rowAt(scrollTop) - OVERSCAN);
  const last = Math.min(p.tracks.length, rowAt(scrollTop + viewHeight) + 1 + OVERSCAN);
  const selectedIndex = p.tracks.findIndex((t) => t.id === p.selectedId);

  // Selection can come from the map, so bring the list entry into view.
  useEffect(() => {
    const el = list.current;
    if (!el || selectedIndex < 0) return;
    const [top, bottom] = [offsets[selectedIndex], offsets[selectedIndex + 1]];
    if (top < el.scrollTop) el.scrollTop = top;
    else if (bottom > el.scrollTop + el.clientHeight) el.scrollTop = bottom - el.clientHeight;
    // Only when the selection changes, not on every list update.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [p.selectedId]);

  const onListKey = (e: KeyboardEvent) => {
    const n = p.tracks.length;
    if (!n) return;
    const page = Math.max(1, Math.floor(viewHeight / rowHeight[""]) - 1);
    const next = {
      ArrowDown: selectedIndex < 0 ? 0 : selectedIndex + 1,
      ArrowUp: selectedIndex < 0 ? 0 : selectedIndex - 1,
      PageDown: selectedIndex + page,
      PageUp: selectedIndex - page,
      Home: 0,
      End: n - 1,
    }[e.key];
    if (next == null) return;
    e.preventDefault();
    p.onSelect(p.tracks[Math.min(n - 1, Math.max(0, next))].id);
  };

  const addFolder = async () => {
    const dir = await open({ directory: true, title: "Add a folder to the library" });
    if (typeof dir !== "string") return;
    try {
      const root = await api.addRoot(dir);
      p.onRootsChanged();
      p.onScan(root.id);
    } catch (e) {
      p.onError(String(e));
    }
  };

  const checkDevices = async () => {
    setDeviceMsg(null);
    const found = await api.listGpsDevices();
    setDevices(found);
    setPicked(new Set(found.flatMap((d) => d.files.filter((f) => !f.imported).map((f) => f.path))));
    if (!found.length) setDeviceMsg("No GPS device with track files found. Is it connected and visible in Finder?");
  };

  const [importing, setImporting] = useState(false);
  const importPicked = async () => {
    if (!p.roots.length) return setDeviceMsg("Add a library folder first.");
    let res;
    setImporting(true);
    try {
      res = await api.importDeviceFiles(p.roots[0].id, [...picked]);
    } catch (e) {
      return setDeviceMsg(String(e));
    } finally {
      setImporting(false);
    }
    setDeviceMsg(
      `Imported ${res.copied.length} file(s) into ${p.roots[0].path}` +
        (res.skipped ? `, ${res.skipped} already known` : "") +
        (res.failed.length ? `. Failed: ${res.failed.join("; ")}` : ""),
    );
    await checkDevices();
    p.onImported();
  };

  const busy = p.scan && p.scan.phase !== "done";
  const unsupported = p.problems.filter((f) => f.status === "unsupported").length;
  const errors = p.problems.filter((f) => f.status === "error").length;
  const missing = p.problems.filter((f) => f.missing).length;

  return (
    <nav className="sidebar">
      <section>
        <header>
          <h2>Library</h2>
          <button onClick={addFolder}>Add folder…</button>
        </header>
        {p.roots.map((r) => (
          <div key={r.id} className="root">
            <span title={r.path}>{r.path}</span>
            <button disabled={!!busy} onClick={() => p.onScan(r.id)} title="Look for new and changed files">
              Sync
            </button>
          </div>
        ))}
        {busy && (
          <div className="progress">
            {p.scan!.phase === "listing" ? "Looking for files…" : `Importing ${p.scan!.done} / ${p.scan!.total}`}
            <progress max={p.scan!.total || 1} value={p.scan!.done} />
          </div>
        )}
        {p.scan?.phase === "done" && (
          <div className="muted">
            Last sync: {p.scan.done} file(s) imported in {(p.scan.elapsedMs / 1000).toFixed(1)} s
          </div>
        )}
        {(unsupported > 0 || errors > 0 || missing > 0) && (
          <button className="link" onClick={() => setShowProblems(!showProblems)}>
            {[
              errors && `${errors} errors`,
              missing && `${missing} missing`,
              unsupported && `${unsupported} unsupported`,
            ]
              .filter(Boolean)
              .join(" · ")}
          </button>
        )}
        {showProblems && (
          <ul className="problems">
            {p.problems.map((f) => (
              <li key={f.path} title={f.error ?? undefined}>
                <b>{f.missing ? "missing" : f.status}</b> {f.path.split("/").slice(-2).join("/")}
                {f.error && <div className="muted">{f.error}</div>}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <header>
          <h2>Device</h2>
          <button onClick={checkDevices}>Check for GPS device</button>
        </header>
        {devices?.map((d) => (
          <div key={d.volume} className="device">
            <div>
              <b>{d.model ?? d.volume.split("/").pop()}</b> <span className="muted">{d.volume}</span>
            </div>
            {d.files.length === 0 && <div className="muted">No track files on the device.</div>}
            <ul>
              {d.files.map((f) => (
                <li key={f.path}>
                  <label className={f.imported ? "muted" : undefined}>
                    <input
                      type="checkbox"
                      disabled={f.imported}
                      checked={picked.has(f.path)}
                      onChange={(e) => {
                        const next = new Set(picked);
                        if (e.target.checked) next.add(f.path);
                        else next.delete(f.path);
                        setPicked(next);
                      }}
                    />
                    {f.name} {f.imported && "(imported)"}
                  </label>
                </li>
              ))}
            </ul>
          </div>
        ))}
        {devices && picked.size > 0 && (
          <button disabled={importing} onClick={importPicked}>
            {importing ? "Importing…" : `Import ${picked.size} file(s)`}
          </button>
        )}
        {deviceMsg && <div className="muted">{deviceMsg}</div>}
      </section>

      <section className="list-section">
        <input
          className="search"
          type="search"
          placeholder={`Search ${p.tracks.length} tracks…`}
          value={p.query}
          onChange={(e) => p.onQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== "ArrowDown" || !p.tracks.length) return;
            e.preventDefault();
            list.current?.focus();
            if (selectedIndex < 0) p.onSelect(p.tracks[0].id);
          }}
        />
        <div
          ref={list}
          className="track-list"
          role="listbox"
          aria-label="Tracks"
          tabIndex={0}
          aria-activedescendant={selectedIndex >= first && selectedIndex < last ? `track-${p.selectedId}` : undefined}
          onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
          onKeyDown={onListKey}
        >
          <div style={{ height: offsets[p.tracks.length], position: "relative" }}>
            {p.tracks.slice(first, last).map((t, k) => (
              <div
                key={t.id}
                id={`track-${t.id}`}
                role="option"
                aria-selected={t.id === p.selectedId}
                className={`list-row ${t.id === p.selectedId ? "selected" : ""}`}
                data-kind={rowKind(t)}
                style={{ top: offsets[first + k], height: offsets[first + k + 1] - offsets[first + k] }}
                onClick={() => p.onSelect(t.id)}
              >
                <div className="track-name">
                  {activityOf(t) && (
                    <span className="activity-icon" title={activityOf(t)!.label}>
                      {activityOf(t)!.icon}
                    </span>
                  )}
                  {displayName(t)}
                </div>
                {routeLine(t) && <div className="track-route">{routeLine(t)}</div>}
                <div className="track-meta">
                  {t.startTime ? new Date(t.startTime).toLocaleDateString() : "no date"} · {formatDistance(t.distanceM)}{" "}
                  · {t.pointCount.toLocaleString()} pts
                  {t.missing && <span className="missing"> · missing</span>}
                  {t.edited && <span> · edited</span>}
                </div>
                {t.tags.length > 0 && (
                  <div className="track-tags">
                    {t.tags.map((g) => (
                      <span key={g} className="tag small">
                        {g}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            ))}
          </div>
        </div>
      </section>
    </nav>
  );
}
