import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
import {
  activityOf,
  api,
  displayName,
  type FileProblem,
  type LastEdit,
  type Line,
  type MapFocus,
  type PointTable,
  type Root,
  type ScanProgress,
  type TrackSummary,
  type WaypointRow,
} from "./api";
import { DEFAULT_MARKER_OPTIONS, type MarkerOptions } from "./components/trackMarkers";
import { MarkerMenu } from "./components/MarkerMenu";
import { DEFAULT_MAP_OPTIONS, MAP_THEMES, MapMenu, TRACK_COLORS, type MapOptions } from "./components/MapMenu";
import { PointMenu } from "./components/PointMenu";
import { EMPTY_SELECTION, sortedIndices, type PointSelection } from "./pointEditing";
import { hoverCursor } from "./hoverCursor";
import { Sidebar } from "./components/Sidebar";
import { MapView } from "./components/MapView";
import { Profile } from "./components/Profile";
import { Details } from "./components/Details";
import { PaneResizer } from "./components/PaneResizer";
import { usePresence } from "./presence";
import "./App.css";

export default function App() {
  const [roots, setRoots] = useState<Root[]>([]);
  const [tracks, setTracks] = useState<TrackSummary[]>([]);
  const [overview, setOverview] = useState<Map<number, Line[]>>(new Map());
  const [problems, setProblems] = useState<FileProblem[]>([]);
  const [allTags, setAllTags] = useState<string[]>([]);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [scan, setScan] = useState<ScanProgress | null>(null);
  const [query, setQuery] = useState("");
  const [showBaseMap, setShowBaseMap] = useState(true);
  const [showAll, setShowAll] = useState(true);
  const [table, setTable] = useState<PointTable | null>(null);
  const [fileWaypoints, setFileWaypoints] = useState<{ fileId: number; rows: WaypointRow[] } | null>(null);
  const [focusedWaypoint, setFocusedWaypoint] = useState<number | null>(null);
  const [lastEdit, setLastEdit] = useState<LastEdit | null>(null);
  const [selection, setSelection] = useState<PointSelection>(EMPTY_SELECTION);
  const [reveal, setReveal] = useState<{ index: number; seq: number } | null>(null);
  const [mapMenu, setMapMenu] = useState<{ x: number; y: number } | null>(null);
  const closeMapMenu = useCallback(() => setMapMenu(null), []);
  const pointSelection = useMemo(() => sortedIndices(selection.set), [selection]);
  const [markerOptions, setMarkerOptions] = useState<MarkerOptions>(loadMarkerOptions);
  const [mapOptions, setMapOptions] = useState<MapOptions>(loadMapOptions);
  const [focus, setFocus] = useState<MapFocus | null>(null);
  // Bumped after point edits so map, profile and point list reload the track's data.
  const [version, setVersion] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = usePaneWidth(SIDEBAR_WIDTH_KEY, SIDEBAR_DEFAULT, SIDEBAR_MIN);
  const [detailsWidth, setDetailsWidth] = usePaneWidth(DETAILS_WIDTH_KEY, DETAILS_DEFAULT, DETAILS_MIN);

  const reloadCatalog = useCallback(async () => {
    try {
      const [t, o, pr, tg] = await Promise.all([
        api.listTracks(),
        api.overview(),
        api.listProblemFiles(),
        api.listTags(),
      ]);
      setTracks(t);
      setOverview(o);
      setProblems(pr);
      setAllTags(tg);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const reloadRoots = useCallback(() => api.listRoots().then(setRoots), []);

  // One sync at a time (the backend refuses a second); roots synced meanwhile wait here.
  const scanQueue = useRef<number[]>([]);
  const scanRunning = useRef(false);
  const startScan = useCallback((rootId: number) => {
    if (scanRunning.current) {
      if (!scanQueue.current.includes(rootId)) scanQueue.current.push(rootId);
      return;
    }
    scanRunning.current = true;
    // Shown as running at once, so the Sync button is disabled before the first event.
    setScan({ ...SCAN_STARTING, rootId });
    api.scanRoot(rootId).catch((e) => scanEnded(String(e)));
  }, []);
  const scanEnded = useCallback(
    (error?: string) => {
      scanRunning.current = false;
      // Also after a failure: files imported before it are in the catalog.
      reloadCatalog().then(() => setVersion((v) => v + 1));
      if (error) {
        setError(error);
        setScan(null);
      }
      const next = scanQueue.current.shift();
      if (next != null) startScan(next);
    },
    [startScan, reloadCatalog],
  );

  useEffect(() => {
    reloadRoots();
    reloadCatalog();
    api
      .refreshPlaces()
      .then((n) => (n > 0 ? reloadCatalog() : undefined))
      .catch((e) => setError(String(e)));
    const unlisten = [
      listen<ScanProgress>("scan-progress", (e) => {
        setScan(e.payload);
        // A rescan may have replaced the selected track's points and waypoints; scanEnded reloads.
        if (e.payload.phase === "done") scanEnded();
      }),
      listen<string>("scan-error", (e) => scanEnded(e.payload)),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, [reloadCatalog, reloadRoots, scanEnded]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return tracks;
    return tracks.filter((t) =>
      [
        displayName(t),
        t.filePath,
        t.description ?? "",
        t.fileDescription ?? "",
        activityOf(t)?.label ?? "",
        ...t.tags,
        ...t.places.flatMap((p) => [p.name, p.region ?? "", p.country ?? ""]),
      ].some((s) => s.toLowerCase().includes(q)),
    );
  }, [tracks, query]);

  const selected = tracks.find((t) => t.id === selectedId) ?? null;

  // The selected track can disappear: a reverted split-off part, or a file a rescan removed.
  useEffect(() => {
    if (selectedId != null && !tracks.some((t) => t.id === selectedId)) setSelectedId(null);
  }, [tracks, selectedId]);

  // While an edit is applied, the shown point indices are about to change; the tree and
  // the map refuse further edits until the new point table has arrived.
  const [editPending, setEditPending] = useState(false);
  const beginEdit = useCallback(() => {
    setSelection(EMPTY_SELECTION);
    setEditPending(true);
  }, []);
  const editFailed = useCallback((message: string) => {
    setError(message);
    setEditPending(false);
  }, []);

  // Indices refer to one version of one track; a new point table invalidates them.
  useEffect(() => setSelection(EMPTY_SELECTION), [table]);

  const selectOnMap = (s: PointSelection) => {
    setSelection(s);
    const index = s.anchor ?? [...s.set][0];
    if (index != null) setReveal((r) => ({ index, seq: (r?.seq ?? 0) + 1 }));
  };

  // One copy of the selected track's points, shared by the point list and the map markers.
  // After an edit the same track's old table stays up until the new one arrives, which
  // keeps the tree's scroll position; edits are locked meanwhile.
  const tableFor = useRef<number | null>(null);
  useEffect(() => {
    let cancelled = false;
    if (tableFor.current !== selectedId) setTable(null);
    if (selectedId == null) {
      tableFor.current = null;
      setEditPending(false);
      api.releaseTrackCache().catch(() => undefined);
      return;
    }
    api
      .pointTable(selectedId)
      .then((t) => {
        if (cancelled) return;
        tableFor.current = selectedId;
        setTable(t);
        setEditPending(false);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
        setEditPending(false);
      });
    return () => {
      cancelled = true;
    };
  }, [selectedId, version]);

  const fileId = selected?.fileId ?? null;
  // Tagged with their file: after an edit the old list stays up until the reload arrives,
  // while another file's list is never shown for this one.
  const waypoints = fileWaypoints?.fileId === fileId ? fileWaypoints.rows : NO_WAYPOINTS;
  useEffect(() => setFocusedWaypoint(null), [fileId]);
  useEffect(() => {
    let cancelled = false;
    if (fileId == null) return;
    api
      .listFileWaypoints(fileId)
      .then((rows) => !cancelled && setFileWaypoints({ fileId, rows }))
      .catch((e) => setError(String(e)));
    return () => {
      cancelled = true;
    };
  }, [fileId, version]);

  useEffect(() => {
    let cancelled = false;
    setLastEdit(null);
    if (fileId == null) return;
    api
      .lastEdit(fileId)
      .then((e) => !cancelled && setLastEdit(e))
      .catch((e) => setError(String(e)));
    return () => {
      cancelled = true;
    };
  }, [fileId, version]);

  // File order first; parts split off in the catalog (negative idx) after, oldest first.
  const fileTracks = useMemo(
    () =>
      tracks
        .filter((t) => t.fileId === fileId)
        .sort((a, b) => (a.idx < 0 ? 1e9 - a.idx : a.idx) - (b.idx < 0 ? 1e9 - b.idx : b.idx)),
    [tracks, fileId],
  );

  const onEdited = async () => {
    await reloadCatalog();
    setVersion((v) => v + 1);
  };

  const lastEditTrack =
    lastEdit && lastEdit.trackId !== selectedId ? (tracks.find((t) => t.id === lastEdit.trackId) ?? null) : null;

  const undo = async () => {
    if (fileId == null || !lastEdit || editPending) return;
    beginEdit();
    try {
      const r = await api.undoEdit(fileId);
      // Show the track that changed: another track of the file, or the one a removed
      // split-off part came from.
      if (selectedId != null && r.restored.length && !r.restored.includes(selectedId)) {
        setSelectedId(r.restored[0]);
      }
      await onEdited();
    } catch (e) {
      editFailed(String(e));
    }
  };

  const undoRef = useRef(undo);
  undoRef.current = undo;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.shiftKey || e.key.toLowerCase() !== "z") return;
      // Text fields keep their own undo.
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName))) return;
      e.preventDefault();
      undoRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const changeMarkerOptions = (o: MarkerOptions) => {
    setMarkerOptions(o);
    savePrefs(MARKER_OPTIONS_KEY, o);
  };

  const changeMapOptions = (o: MapOptions) => {
    setMapOptions(o);
    savePrefs(MAP_OPTIONS_KEY, o);
  };

  // The details pane stays for its slide-out after the selection is cleared.
  const detailsPane = usePresence(selected, PANE_MS);
  const detailsClosing = detailsPane.closing;
  // A fading menu keeps the selection it was opened for; an edit clears it right away.
  const mapMenuShown = usePresence(
    mapMenu && selected && table && { ...mapMenu, trackId: selected.id, table, selection: pointSelection },
  );
  const errorShown = usePresence(error);

  // Stored widths are preferences; what is shown is limited so the map keeps MAP_MIN when
  // the window is small or the details pane opens, and the details pane gives way last.
  const hasDetails = detailsPane.shown != null;
  const windowWidth = useWindowWidth(sidebarWidth + (hasDetails ? detailsWidth : 0) + MAP_MIN);
  const room = windowWidth - MAP_MIN;
  const shownDetails = hasDetails ? clampWidth(detailsWidth, DETAILS_MIN, room - SIDEBAR_MIN) : 0;
  const shownSidebar = clampWidth(sidebarWidth, SIDEBAR_MIN, room - shownDetails);
  // Resizing one pane also keeps the other as shown, so a pane squeezed earlier does not
  // spring back to its old preference when the user makes room for the map.
  const resizeSidebar = (w: number) => {
    setSidebarWidth(w);
    if (hasDetails) setDetailsWidth(shownDetails);
  };
  const resizeDetails = (w: number) => {
    setDetailsWidth(w);
    setSidebarWidth(shownSidebar);
  };

  const patchTrack = (id: number, patch: Partial<TrackSummary>) =>
    setTracks((ts) => ts.map((t) => (t.id === id ? { ...t, ...patch } : t)));

  // Kept as an element so the closing pane shows what it showed last, not an empty track.
  const detailsElement = useRef<ReactNode>(null);
  if (selected) {
    detailsElement.current = (
      <Details
        track={selected}
        allTags={allTags}
        onSaveMeta={async (title, description) => {
          await api.updateTrackMeta(selected.id, title, description);
          patchTrack(selected.id, { title: title.trim() || null, description: description.trim() || null });
        }}
        onSetTags={async (tags) => {
          await api.setTrackTags(selected.id, tags);
          patchTrack(selected.id, { tags });
          setAllTags(await api.listTags());
        }}
        onSetActivity={async (activity) => {
          await api.setTrackActivity(selected.id, activity);
          patchTrack(selected.id, { activity });
        }}
        table={table}
        fileTracks={fileTracks}
        waypoints={waypoints}
        focusedWaypoint={focusedWaypoint}
        onSelectTrack={setSelectedId}
        onFocusWaypoint={setFocusedWaypoint}
        onHover={hoverCursor.set}
        selection={selection}
        onSelectionChange={setSelection}
        reveal={reveal}
        onZoomTo={(bbox) => setFocus((f) => ({ bbox, seq: (f?.seq ?? 0) + 1 }))}
        onPanTo={(center) => setFocus((f) => ({ center, seq: (f?.seq ?? 0) + 1 }))}
        locked={editPending}
        onBeginEdit={beginEdit}
        onEdited={onEdited}
        onError={editFailed}
        lastEdit={lastEdit}
        lastEditTrack={lastEditTrack}
        onUndo={undo}
      />
    );
  } else if (!hasDetails) detailsElement.current = null;

  return (
    <div
      className={`app${detailsClosing ? " details-closing" : ""}`}
      style={
        {
          "--sidebar-w": `${shownSidebar}px`,
          // The column shrinks to nothing while the closing pane keeps its width.
          "--details-w": `${detailsClosing ? 0 : shownDetails}px`,
          "--details-pane-w": `${shownDetails}px`,
        } as CSSProperties
      }
    >
      <Sidebar
        roots={roots}
        tracks={filtered}
        selectedId={selectedId}
        scan={scan}
        problems={problems}
        query={query}
        onQuery={setQuery}
        onSelect={setSelectedId}
        onRootsChanged={reloadRoots}
        onScan={startScan}
        onImported={reloadCatalog}
        onError={setError}
      />
      <PaneResizer
        side="left"
        label="Resize library pane"
        cssVar="--sidebar-w"
        width={shownSidebar}
        defaultWidth={SIDEBAR_DEFAULT}
        min={SIDEBAR_MIN}
        max={() => window.innerWidth - MAP_MIN - shownDetails}
        onChange={resizeSidebar}
      />
      <main>
        <div className="toolbar">
          <strong title={selected ? displayName(selected) : undefined}>
            {selected ? displayName(selected) : `${filtered.length} tracks`}
          </strong>
          <div className="toggles">
            <MarkerMenu options={markerOptions} onChange={changeMarkerOptions} />
            <MapMenu options={mapOptions} onChange={changeMapOptions} />
            <label title="Show every track in the list (search narrows it down)">
              <input type="checkbox" checked={showAll} onChange={(e) => setShowAll(e.target.checked)} /> All tracks
            </label>
            <label>
              <input type="checkbox" checked={showBaseMap} onChange={(e) => setShowBaseMap(e.target.checked)} /> Base
              map
            </label>
          </div>
        </div>
        {errorShown.shown && (
          <div className={errorShown.closing ? "error closing" : "error"} onClick={() => setError(null)}>
            {errorShown.shown}
          </div>
        )}
        <MapView
          tracks={filtered}
          overview={overview}
          selected={selected}
          showBaseMap={showBaseMap}
          mapOptions={mapOptions}
          showAll={showAll}
          table={table}
          pointSelection={pointSelection}
          markerOptions={markerOptions}
          waypoints={waypoints}
          focusedWaypoint={focusedWaypoint}
          focus={focus}
          version={version}
          onSelect={setSelectedId}
          pointView={markerOptions.pointView}
          selection={selection}
          onPointSelection={selectOnMap}
          onPointMenu={(x, y) => !editPending && setMapMenu({ x, y })}
          locked={editPending}
        />
        {mapMenuShown.shown && (
          <PointMenu
            key={`${mapMenuShown.shown.x},${mapMenuShown.shown.y}`}
            x={mapMenuShown.shown.x}
            y={mapMenuShown.shown.y}
            closing={mapMenuShown.closing}
            trackId={mapMenuShown.shown.trackId}
            table={mapMenuShown.shown.table}
            selection={mapMenuShown.shown.selection}
            onClose={closeMapMenu}
            onZoomTo={(bbox) => setFocus((f) => ({ bbox, seq: (f?.seq ?? 0) + 1 }))}
            onBeginEdit={beginEdit}
            onEdited={onEdited}
            onError={editFailed}
          />
        )}
        <Profile track={selected} version={version} onCursor={hoverCursor.set} />
      </main>
      {selected && (
        <PaneResizer
          side="right"
          label="Resize details pane"
          cssVar="--details-w"
          width={shownDetails}
          defaultWidth={DETAILS_DEFAULT}
          min={DETAILS_MIN}
          max={() => window.innerWidth - MAP_MIN - shownSidebar}
          onChange={resizeDetails}
        />
      )}
      {hasDetails && (
        // `display: contents`, so the pane stays a grid item; inert keeps focus and keys out
        // of a pane that is sliding away with a track that no longer exists.
        <div className="details-slot" inert={detailsClosing}>
          {detailsElement.current}
        </div>
      )}
    </div>
  );
}

const NO_WAYPOINTS: WaypointRow[] = [];
const SCAN_STARTING: ScanProgress = {
  rootId: 0,
  phase: "listing",
  total: 0,
  done: 0,
  current: null,
  tracks: 0,
  errors: 0,
  unsupported: 0,
  missing: 0,
  elapsedMs: 0,
};
const MARKER_OPTIONS_KEY = "markerOptions";
const MAP_OPTIONS_KEY = "mapOptions";

function savePrefs(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Preferences are a convenience; without storage they just reset on restart.
  }
}

function loadMapOptions(): MapOptions {
  try {
    const raw = localStorage.getItem(MAP_OPTIONS_KEY);
    if (raw) {
      const o: MapOptions = { ...DEFAULT_MAP_OPTIONS, ...JSON.parse(raw) };
      // Unknown stored values would break the style lookup or line paint.
      if (!Object.prototype.hasOwnProperty.call(TRACK_COLORS, o.trackColor)) {
        o.trackColor = DEFAULT_MAP_OPTIONS.trackColor;
      }
      if (!MAP_THEMES.some((t) => t.value === o.theme)) o.theme = DEFAULT_MAP_OPTIONS.theme;
      return o;
    }
  } catch {
    // Fall through to defaults.
  }
  return DEFAULT_MAP_OPTIONS;
}

function loadMarkerOptions(): MarkerOptions {
  try {
    const raw = localStorage.getItem(MARKER_OPTIONS_KEY);
    if (raw) return { ...DEFAULT_MARKER_OPTIONS, ...JSON.parse(raw) };
  } catch {
    // Fall through to defaults.
  }
  return DEFAULT_MARKER_OPTIONS;
}

const SIDEBAR_WIDTH_KEY = "sidebarWidth";
const DETAILS_WIDTH_KEY = "detailsWidth";
const SIDEBAR_DEFAULT = 300;
const DETAILS_DEFAULT = 340;
const SIDEBAR_MIN = 200;
// The point table's fixed columns plus a full "lat, lon" pair need about this much.
const DETAILS_MIN = 310;
const MAP_MIN = 320;
/**
 * How long the closing details pane stays mounted: a little longer than `--pane` in
 * App.css, since the transition starts at the next style update, after this timer.
 */
const PANE_MS = 240;

const clampWidth = (w: number, min: number, max: number) => Math.max(min, Math.min(max, w));

/**
 * The window width, capped at `cap`: resizes above the cap change nothing visible, so they
 * skip re-rendering the app.
 */
function useWindowWidth(cap: number) {
  const [, rerender] = useReducer((n: number) => n + 1, 0);
  const width = Math.min(window.innerWidth, cap);
  const shown = useRef(width);
  shown.current = width;
  useEffect(() => {
    const onResize = () => {
      if (Math.min(window.innerWidth, cap) !== shown.current) rerender();
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [cap]);
  return width;
}

function usePaneWidth(key: string, fallback: number, min: number) {
  const [width, setWidth] = useState(() => {
    try {
      const n = Number(localStorage.getItem(key));
      if (Number.isFinite(n) && n >= min) return n;
    } catch {
      // Fall through to the default.
    }
    return fallback;
  });
  const change = useCallback(
    (w: number) => {
      setWidth(w);
      try {
        localStorage.setItem(key, String(w));
      } catch {
        // Widths are a convenience; without storage they just reset on restart.
      }
    },
    [key],
  );
  return [width, change] as const;
}
