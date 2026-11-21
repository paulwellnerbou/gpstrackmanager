import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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
import { PointMenu } from "./components/PointMenu";
import { EMPTY_SELECTION, type PointSelection } from "./pointEditing";
import { Sidebar } from "./components/Sidebar";
import { MapView } from "./components/MapView";
import { Profile } from "./components/Profile";
import { Details } from "./components/Details";
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
  const [lastEdit, setLastEdit] = useState<LastEdit | null>(null);
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
        // A rescan may have replaced the selected track's points and waypoints.
        if (e.payload.phase === "done") reloadCatalog().then(() => setVersion((v) => v + 1));
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
    try {
      localStorage.setItem(MARKER_OPTIONS_KEY, JSON.stringify(o));
    } catch {
      // Preferences are a convenience; without storage they just reset on restart.
    }
  };

  const patchTrack = (id: number, patch: Partial<TrackSummary>) =>
    setTracks((ts) => ts.map((t) => (t.id === id ? { ...t, ...patch } : t)));

  return (
    <div className={`app ${selected ? "with-details" : ""}`}>
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
        onError={setError}
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
          onPointMenu={(x, y) => !editPending && setMapMenu({ x, y })}
          locked={editPending}
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
            onBeginEdit={beginEdit}
            onEdited={onEdited}
            onError={editFailed}
          />
        )}
        <Profile track={selected} version={version} onCursor={setCursor} />
      </main>
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
          locked={editPending}
          onBeginEdit={beginEdit}
          onEdited={onEdited}
          onError={editFailed}
          lastEdit={lastEdit}
          lastEditTrack={lastEditTrack}
          onUndo={undo}
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
