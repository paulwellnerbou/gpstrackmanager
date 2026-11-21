import { useEffect, useRef, useState } from "react";
import { Map as MapLibreMap, setWorkerUrl, type StyleSpecification, type GeoJSONSource } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?url";
import type { Feature, GeoJSON as GeoJSONData } from "geojson";
import { api, type Line, type MapFocus, type PointTable, type TrackSummary, type WaypointRow } from "../api";
import { TrackMarkers, type MarkerOptions } from "./trackMarkers";
import { useColorScheme } from "../useColorScheme";
import { clickSelection, isToggleKey, type PointSelection } from "../pointEditing";
import { hoverCursor } from "../hoverCursor";
import { GeoSearch } from "./GeoSearch";
import { installBoxSelect, pointAt, pointsInRect, updateDots, type PointView } from "./mapPoints";

// MapLibre locates its worker relative to its own module URL, which Vite's dependency
// pre-bundling relocates; point it at the real file instead.
setWorkerUrl(workerUrl);

const MAP_STYLES = {
  light: "https://tiles.openfreemap.org/styles/liberty",
  dark: "https://tiles.openfreemap.org/styles/dark",
};
const blankStyle = (scheme: "light" | "dark"): StyleSpecification => ({
  version: 8,
  sources: {},
  layers: [
    {
      id: "background",
      type: "background",
      paint: { "background-color": scheme === "dark" ? "#1e1d1b" : "#f3f1ec" },
    },
  ],
});

const OWN_SOURCES = ["overview", "detail", "dots", "highlight", "cursor"] as const;
/** Drawing more highlighted points than this adds nothing but load. */
const MAX_HIGHLIGHT = 5000;
const OVERVIEW_COLOR = "#7a5cc4";
const OVERVIEW_DIMMED = "#9c97a8";

interface Props {
  tracks: TrackSummary[];
  overview: Map<number, Line[]>;
  selected: TrackSummary | null;
  showBaseMap: boolean;
  showAll: boolean;
  /** Point table of the selected track, for markers and highlighted points. */
  table: PointTable | null;
  /** Selected point indices, ascending. */
  pointSelection: number[];
  markerOptions: MarkerOptions;
  /** Waypoints of the selected track's file. */
  waypoints: WaypointRow[];
  focusedWaypoint: number | null;
  focus: MapFocus | null;
  version: number;
  onSelect: (id: number) => void;
  pointView: PointView;
  selection: PointSelection;
  /** Selection made on the map. */
  onPointSelection: (s: PointSelection) => void;
  /** Right-click on the map with points selected; client coordinates. */
  onPointMenu: (x: number, y: number) => void;
  /** An edit is being applied: no point selection on the map meanwhile. */
  locked: boolean;
}

function multiLine(lines: Line[], id?: number): Feature {
  return { type: "Feature", id, properties: {}, geometry: { type: "MultiLineString", coordinates: lines } };
}

function addOwnLayers(map: MapLibreMap) {
  for (const id of OWN_SOURCES) {
    if (!map.getSource(id)) map.addSource(id, { type: "geojson", data: { type: "FeatureCollection", features: [] } });
  }
  map.addLayer({
    id: "overview",
    type: "line",
    source: "overview",
    paint: { "line-color": OVERVIEW_COLOR, "line-width": 1.5, "line-opacity": 0.55 },
  });
  map.addLayer({
    id: "detail-casing",
    type: "line",
    source: "detail",
    layout: { "line-join": "round", "line-cap": "round" },
    paint: { "line-color": "#ffffff", "line-width": 8 },
  });
  map.addLayer({
    id: "detail",
    type: "line",
    source: "detail",
    layout: { "line-join": "round", "line-cap": "round" },
    paint: { "line-color": "#d6336c", "line-width": 4 },
  });
  map.addLayer({
    id: "dots",
    type: "circle",
    source: "dots",
    paint: {
      "circle-radius": ["interpolate", ["linear"], ["zoom"], 12, 2.5, 18, 4.5],
      "circle-color": "#d6336c",
      "circle-stroke-color": "#ffffff",
      "circle-stroke-width": 1,
    },
  });
  map.addLayer({
    id: "highlight",
    type: "circle",
    source: "highlight",
    paint: { "circle-radius": 4, "circle-color": "#f59f00", "circle-stroke-color": "#fff", "circle-stroke-width": 1 },
  });
  map.addLayer({
    id: "cursor",
    type: "circle",
    source: "cursor",
    paint: { "circle-radius": 6, "circle-color": "#d6336c", "circle-stroke-color": "#fff", "circle-stroke-width": 2 },
  });
}

const styleCache = new Map<string, Promise<StyleSpecification>>();

function fetchStyle(url: string): Promise<StyleSpecification> {
  let p = styleCache.get(url);
  if (!p) {
    p = fetch(url).then((r) => {
      if (!r.ok) throw new Error(`${r.status}`);
      return r.json() as Promise<StyleSpecification>;
    });
    // A failed fetch is retried next time, e.g. once the network is back.
    p.catch(() => styleCache.delete(url));
    styleCache.set(url, p);
  }
  return p;
}

/** Paint/layout changes throw while a new style is still being built; wait for it. */
function whenStyleReady(map: MapLibreMap, fn: () => void) {
  if (map.isStyleLoaded()) fn();
  else map.once("idle", fn);
}

function setData(map: MapLibreMap, id: string, data: GeoJSONData) {
  (map.getSource(id) as GeoJSONSource | undefined)?.setData(data);
}

export function MapView({
  tracks,
  overview,
  selected,
  showBaseMap,
  showAll,
  table,
  pointSelection,
  markerOptions,
  waypoints,
  focusedWaypoint,
  focus,
  version,
  onSelect,
  pointView,
  selection,
  onPointSelection,
  onPointMenu,
  locked,
}: Props) {
  const container = useRef<HTMLDivElement>(null);
  const mapRef = useRef<MapLibreMap | null>(null);
  const selectedRef = useRef<TrackSummary | null>(null);
  const requestSeq = useRef(0);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const [ready, setReady] = useState(false);
  const scheme = useColorScheme();
  const [baseMapFailed, setBaseMapFailed] = useState(false);
  const markers = useRef<TrackMarkers | null>(null);
  const [tooManyDots, setTooManyDots] = useState<number | null>(null);
  // Map event handlers are registered once; they read the current props through these.
  const live = useRef({ table, selection, pointView, onPointSelection, onPointMenu, locked });
  live.current = { table, selection, pointView, onPointSelection, onPointMenu, locked };
  const refreshDots = useRef(() => {
    const map = mapRef.current;
    if (map) setTooManyDots(updateDots(map, live.current.table, live.current.pointView));
  });

  // Detail is re-fetched for every view change: the backend returns only what is
  // visible, thinned to the current zoom, so huge tracks never reach the renderer whole.
  const refreshDetail = useRef(async () => {
    const map = mapRef.current;
    const t = selectedRef.current;
    if (!map || !map.getSource("detail")) return;
    if (!t) return setData(map, "detail", { type: "FeatureCollection", features: [] });
    const seq = ++requestSeq.current;
    const b = map.getBounds();
    const padX = (b.getEast() - b.getWest()) * 0.25;
    const padY = (b.getNorth() - b.getSouth()) * 0.25;
    const bbox: [number, number, number, number] = [
      b.getWest() - padX,
      b.getSouth() - padY,
      b.getEast() + padX,
      b.getNorth() + padY,
    ];
    const lines = await api.trackPoints(t.id, map.getZoom(), bbox);
    if (seq === requestSeq.current) setData(map, "detail", multiLine(lines));
  });

  useEffect(() => {
    const map = new MapLibreMap({
      container: container.current!,
      // Start with the local style so our layers exist even without network; the base map
      // is loaded on top once its style could be fetched.
      style: blankStyle(scheme),
      center: [10, 50],
      zoom: 4,
      attributionControl: { compact: true },
    });
    mapRef.current = map;
    map.on("load", () => {
      addOwnLayers(map);
      markers.current = new TrackMarkers(map);
      setReady(true);
      refreshDetail.current();
      refreshDots.current();
    });
    map.on("moveend", () => {
      refreshDetail.current();
      refreshDots.current();
    });
    installBoxSelect(
      map,
      () => live.current.table != null && !live.current.locked,
      (a, b, additive) => {
        const { table: t, selection: cur } = live.current;
        if (!t) return;
        const hits = pointsInRect(map, t, a, b);
        const set = additive ? new Set(cur.set) : new Set<number>();
        for (const i of hits) set.add(i);
        live.current.onPointSelection({ set, anchor: hits[0] ?? cur.anchor });
      },
    );
    const dotAt = (p: { x: number; y: number }) =>
      live.current.pointView === "line" || live.current.locked ? null : pointAt(map, p);
    // Thin lines are hard to hit exactly, so pick within a few pixels of the pointer.
    const trackAt = (p: { x: number; y: number }) => {
      const r = 6;
      const hits = map.getLayer("overview")
        ? map.queryRenderedFeatures(
            [
              [p.x - r, p.y - r],
              [p.x + r, p.y + r],
            ],
            { layers: ["overview"] },
          )
        : [];
      const id = hits[0]?.id;
      return typeof id === "number" ? id : null;
    };
    map.on("click", (e) => {
      const i = dotAt(e.point);
      if (i != null) {
        const ev = e.originalEvent;
        const mods = { shift: ev.shiftKey, toggle: isToggleKey(ev) };
        live.current.onPointSelection(clickSelection(live.current.selection, i, mods));
        return;
      }
      const id = trackAt(e.point);
      if (id != null) onSelectRef.current(id);
    });
    map.on("contextmenu", (e) => {
      e.originalEvent.preventDefault();
      const i = dotAt(e.point);
      let sel = live.current.selection;
      if (i != null && !sel.set.has(i)) {
        sel = { set: new Set([i]), anchor: i };
        live.current.onPointSelection(sel);
      }
      if (sel.set.size) live.current.onPointMenu(e.originalEvent.clientX, e.originalEvent.clientY);
    });
    map.on("mousemove", (e) => {
      map.getCanvas().style.cursor = dotAt(e.point) != null || trackAt(e.point) != null ? "pointer" : "";
    });
    return () => map.remove();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !ready) return;
    let cancelled = false;
    // transformStyle carries our sources and layers over to the new base style.
    const apply = (style: StyleSpecification) =>
      map.setStyle(style, {
        transformStyle: (prev, next) => {
          if (!prev) return next;
          const own = prev.layers.filter(
            (l) => "source" in l && (OWN_SOURCES as readonly string[]).includes(l.source as string),
          );
          const sources = { ...next.sources };
          for (const id of OWN_SOURCES) if (prev.sources[id]) sources[id] = prev.sources[id];
          return { ...next, sources, layers: [...next.layers, ...own] };
        },
      });
    if (!showBaseMap) {
      apply(blankStyle(scheme));
      setBaseMapFailed(false);
      return;
    }
    // Fetched here rather than by MapLibre so that, offline, the map keeps working on the
    // local style instead of ending up with no style at all.
    fetchStyle(MAP_STYLES[scheme])
      .then((style) => {
        if (cancelled) return;
        apply(style);
        setBaseMapFailed(false);
      })
      .catch(() => {
        if (cancelled) return;
        apply(blankStyle(scheme));
        setBaseMapFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [showBaseMap, scheme, ready]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !ready) return;
    const features = showAll
      ? tracks
          .filter((t) => t.id !== selected?.id && overview.has(t.id))
          .map((t) => multiLine(overview.get(t.id)!, t.id))
      : [];
    setData(map, "overview", { type: "FeatureCollection", features });
  }, [tracks, overview, selected?.id, ready, showAll]);

  // The other tracks fade while one is selected, so even a tiny selected track stands out.
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !ready) return;
    whenStyleReady(map, () => {
      map.setPaintProperty("overview", "line-color", selected ? OVERVIEW_DIMMED : OVERVIEW_COLOR);
      map.setPaintProperty("overview", "line-opacity", selected ? 0.35 : 0.55);
    });
  }, [selected?.id, ready]);

  useEffect(() => {
    selectedRef.current = selected;
    const map = mapRef.current;
    if (!map) return;
    const b = selected?.bbox;
    // Show the coarse overview geometry at once; the detailed fetch replaces it.
    const coarse = selected && overview.get(selected.id);
    if (coarse && map.getSource("detail")) setData(map, "detail", multiLine(coarse));
    refreshDetail.current();
    if (!b) return;
    // Selecting can open the details panel and shrink the map; a resize mid-animation
    // cancels fitBounds, so fit once the new layout is in place.
    const frame = requestAnimationFrame(() => {
      map.resize();
      map.fitBounds(
        [
          [b[1], b[0]],
          [b[3], b[2]],
        ],
        { padding: 60, maxZoom: 16, duration: 400 },
      );
    });
    return () => cancelAnimationFrame(frame);
    // Only re-run when a different track is selected, not when its metadata is edited.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected?.id]);

  useEffect(() => {
    refreshDetail.current();
  }, [version]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !ready) return;
    const coords: [number, number][] = [];
    if (table) {
      const step = Math.max(1, Math.ceil(pointSelection.length / MAX_HIGHLIGHT));
      for (let k = 0; k < pointSelection.length; k += step) {
        const i = pointSelection[k];
        if (i < table.length) coords.push([table.lon[i], table.lat[i]]);
      }
    }
    setData(map, "highlight", {
      type: "FeatureCollection",
      features: coords.length
        ? [{ type: "Feature", properties: {}, geometry: { type: "MultiPoint", coordinates: coords } }]
        : [],
    });
    markers.current?.update(table, pointSelection, markerOptions, waypoints, focusedWaypoint);
  }, [table, pointSelection, markerOptions, waypoints, focusedWaypoint, ready]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !focus) return;
    if ("center" in focus) {
      map.easeTo({ center: focus.center, duration: 300 });
      return;
    }
    const [x0, y0, x1, y1] = focus.bbox;
    map.fitBounds(
      [
        [x0, y0],
        [x1, y1],
      ],
      { padding: 80, maxZoom: 18, duration: 500 },
    );
  }, [focus]);

  useEffect(
    () =>
      hoverCursor.subscribe((cursor) => {
        const map = mapRef.current;
        if (!map || !map.getSource("cursor")) return;
        setData(map, "cursor", {
          type: "FeatureCollection",
          features: cursor
            ? [{ type: "Feature", properties: {}, geometry: { type: "Point", coordinates: cursor } }]
            : [],
        });
      }),
    [],
  );

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !ready) return;
    const line = pointView === "points" ? "none" : "visible";
    whenStyleReady(map, () => {
      map.setLayoutProperty("detail", "visibility", line);
      map.setLayoutProperty("detail-casing", "visibility", line);
    });
    refreshDots.current();
  }, [table, pointView, ready]);

  return (
    <div className="map-wrap">
      <div ref={container} className="map" />
      <GeoSearch map={ready ? mapRef.current : null} />
      {baseMapFailed && <div className="map-hint map-hint-bottom">Base map unavailable (offline?)</div>}
      {tooManyDots != null && (
        <div className="map-hint">Zoom in to see points ({tooManyDots.toLocaleString()} in view)</div>
      )}
    </div>
  );
}
