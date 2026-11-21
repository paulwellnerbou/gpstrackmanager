import { useCallback, useEffect, useMemo, useState, type CSSProperties } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  activityOf,
  api,
  displayName,
  type FileProblem,
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
import { PointMenu } from "./components/PointMenu";
import { EMPTY_SELECTION, type PointSelection } from "./pointEditing";
import { Sidebar } from "./components/Sidebar";
import { MapView } from "./components/MapView";
import { Profile } from "./components/Profile";
import { Details } from "./components/Details";
import { PaneResizer } from "./components/PaneResizer";
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
  const [cursor, setCursor] = useState<[number, number] | null>(null);
  const [table, setTable] = useState<PointTable | null>(null);
  const [waypoints, setWaypoints] = useState<WaypointRow[]>([]);
  const [focusedWaypoint, setFocusedWaypoint] = useState<number | null>(null);
  const [selection, setSelection] = useState<PointSelection>(EMPTY_SELECTION);
  const [reveal, setReveal] = useState<{ index: number; seq: number } | null>(null);
  const [mapMenu, setMapMenu] = useState<{ x: number; y: number } | null>(null);
  const closeMapMenu = useCallback(() => setMapMenu(null), []);
  const pointSelection = useMemo(() => [...selection.set].sort((a, b) => a - b), [selection]);
  const [markerOptions, setMarkerOptions] = useState<MarkerOptions>(loadMarkerOptions);
  const [focus, setFocus] = useState<MapFocus | null>(null);
  // Bumped after point edits so map, profile and point list reload the track's data.
  const [version, setVersion] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = usePaneWidth(SIDEBAR_WIDTH_KEY, SIDEBAR_DEFAULT);
  const [detailsWidth, setDetailsWidth] = usePaneWidth(DETAILS_WIDTH_KEY, DETAILS_DEFAULT);

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

  useEffect(() => {
    reloadRoots();
    reloadCatalog();
    const unlisten = [
      listen<ScanProgress>("scan-progress", (e) => {
        setScan(e.payload);
        if (e.payload.phase === "done") reloadCatalog();
      }),
      listen<string>("scan-error", (e) => setError(e.payload)),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  }, [reloadCatalog, reloadRoots]);

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
      ].some((s) => s.toLowerCase().includes(q)),
    );
  }, [tracks, query]);

  const selected = tracks.find((t) => t.id === selectedId) ?? null;

  // Indices refer to one version of one track; a new point table invalidates them.
  useEffect(() => setSelection(EMPTY_SELECTION), [table]);

  const selectOnMap = (s: PointSelection) => {
    setSelection(s);
    const index = s.anchor ?? [...s.set][0];
    if (index != null) setReveal((r) => ({ index, seq: (r?.seq ?? 0) + 1 }));
  };

  // One copy of the selected track's points, shared by the point list and the map markers.
  useEffect(() => {
    let cancelled = false;
    setTable(null);
    if (selectedId == null) return;
    api
      .pointTable(selectedId)
      .then((t) => !cancelled && setTable(t))
      .catch((e) => setError(String(e)));
    return () => {
      cancelled = true;
    };
  }, [selectedId, version]);

  const fileId = selected?.fileId ?? null;
  useEffect(() => {
    let cancelled = false;
    setWaypoints([]);
    setFocusedWaypoint(null);
    if (fileId == null) return;
    api
      .listFileWaypoints(fileId)
      .then((w) => !cancelled && setWaypoints(w))
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

  const changeMarkerOptions = (o: MarkerOptions) => {
    setMarkerOptions(o);
    try {
      localStorage.setItem(MARKER_OPTIONS_KEY, JSON.stringify(o));
    } catch {
      // Preferences are a convenience; without storage they just reset on restart.
    }
  };

  const patchTrack = (id: number, patch: Partial<TrackSummary>) =>
    setTracks((ts) => ts.map((t) => (t.id === id ? { ...t, ...patch } : t)));

  return (
    <div
      className={`app ${selected ? "with-details" : ""}`}
      style={{ "--sidebar-w": `${sidebarWidth}px`, "--details-w": `${detailsWidth}px` } as CSSProperties}
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
        onScan={(id) => api.scanRoot(id).catch((e) => setError(String(e)))}
        onImported={reloadCatalog}
      />
      <PaneResizer
        side="left"
        cssVar="--sidebar-w"
        width={sidebarWidth}
        defaultWidth={SIDEBAR_DEFAULT}
        min={PANE_MIN}
        max={() => window.innerWidth - (selected ? detailsWidth : 0) - MAP_MIN}
        onChange={setSidebarWidth}
      />
      <main>
        <div className="toolbar">
          <strong>{selected ? displayName(selected) : `${filtered.length} tracks`}</strong>
          <div className="toggles">
            <MarkerMenu options={markerOptions} onChange={changeMarkerOptions} />
            <label title="Show every track in the list (search narrows it down)">
              <input type="checkbox" checked={showAll} onChange={(e) => setShowAll(e.target.checked)} /> All tracks
            </label>
            <label>
              <input type="checkbox" checked={showBaseMap} onChange={(e) => setShowBaseMap(e.target.checked)} /> Base
              map
            </label>
          </div>
        </div>
        {error && (
          <div className="error" onClick={() => setError(null)}>
            {error}
          </div>
        )}
        <MapView
          tracks={filtered}
          overview={overview}
          selected={selected}
          showBaseMap={showBaseMap}
          showAll={showAll}
          cursor={cursor}
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
          onPointMenu={(x, y) => setMapMenu({ x, y })}
        />
        {mapMenu && selected && table && (
          <PointMenu
            x={mapMenu.x}
            y={mapMenu.y}
            trackId={selected.id}
            table={table}
            selection={pointSelection}
            onClose={closeMapMenu}
            onZoomTo={(bbox) => setFocus((f) => ({ bbox, seq: (f?.seq ?? 0) + 1 }))}
            onEdited={onEdited}
            onError={setError}
          />
        )}
        <Profile track={selected} version={version} onCursor={setCursor} />
      </main>
      {selected && (
        <PaneResizer
          side="right"
          cssVar="--details-w"
          width={detailsWidth}
          defaultWidth={DETAILS_DEFAULT}
          min={PANE_MIN}
          max={() => window.innerWidth - sidebarWidth - MAP_MIN}
          onChange={setDetailsWidth}
        />
      )}
      {selected && (
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
          onHover={setCursor}
          selection={selection}
          onSelectionChange={setSelection}
          reveal={reveal}
          onZoomTo={(bbox) => setFocus((f) => ({ bbox, seq: (f?.seq ?? 0) + 1 }))}
          onPanTo={(center) => setFocus((f) => ({ center, seq: (f?.seq ?? 0) + 1 }))}
          onEdited={onEdited}
          onError={setError}
        />
      )}
    </div>
  );
}

const MARKER_OPTIONS_KEY = "markerOptions";

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
const PANE_MIN = 200;
const MAP_MIN = 320;

function usePaneWidth(key: string, fallback: number) {
  const [width, setWidth] = useState(() => {
    try {
      const n = Number(localStorage.getItem(key));
      if (Number.isFinite(n) && n >= PANE_MIN) return n;
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
