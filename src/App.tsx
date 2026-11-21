import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  activityOf,
  api,
  displayName,
  type FileProblem,
  type Line,
  type PointTable,
  type Root,
  type ScanProgress,
  type TrackSummary,
} from "./api";
import { DEFAULT_MARKER_OPTIONS, type MarkerOptions } from "./components/trackMarkers";
import { MarkerMenu } from "./components/MarkerMenu";
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
  const [pointSelection, setPointSelection] = useState<number[]>([]);
  const [markerOptions, setMarkerOptions] = useState<MarkerOptions>(loadMarkerOptions);
  const [focus, setFocus] = useState<{ bbox: [number, number, number, number]; seq: number } | null>(null);
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
          focus={focus}
          version={version}
          onSelect={setSelectedId}
        />
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
          onHover={setCursor}
          onSelectionChange={setPointSelection}
          onZoomTo={(bbox) => setFocus((f) => ({ bbox, seq: (f?.seq ?? 0) + 1 }))}
          onEdited={async () => {
            await reloadCatalog();
            setVersion((v) => v + 1);
          }}
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
