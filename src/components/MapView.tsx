import { useEffect, useRef, useState } from "react";
import { Map as MapLibreMap, setWorkerUrl, type StyleSpecification, type GeoJSONSource } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?url";
import type { Feature, GeoJSON as GeoJSONData } from "geojson";
import { api, type Line, type TrackSummary } from "../api";

// MapLibre locates its worker relative to its own module URL, which Vite's dependency
// pre-bundling relocates; point it at the real file instead.
setWorkerUrl(workerUrl);

const MAP_STYLE = "https://tiles.openfreemap.org/styles/liberty";
const BLANK_STYLE: StyleSpecification = {
  version: 8,
  sources: {},
  layers: [{ id: "background", type: "background", paint: { "background-color": "#f3f1ec" } }],
};

const OWN_SOURCES = ["overview", "detail", "highlight", "cursor"] as const;

interface Props {
  tracks: TrackSummary[];
  overview: Map<number, Line[]>;
  selected: TrackSummary | null;
  showBaseMap: boolean;
  showAll: boolean;
  cursor: [number, number] | null;
  highlight: [number, number][];
  focus: { bbox: [number, number, number, number]; seq: number } | null;
  version: number;
  onSelect: (id: number) => void;
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
    paint: { "line-color": "#7a5cc4", "line-width": 1.5, "line-opacity": 0.55 },
  });
  map.addLayer({
    id: "detail-casing",
    type: "line",
    source: "detail",
    layout: { "line-join": "round", "line-cap": "round" },
    paint: { "line-color": "#ffffff", "line-width": 6 },
  });
  map.addLayer({
    id: "detail",
    type: "line",
    source: "detail",
    layout: { "line-join": "round", "line-cap": "round" },
    paint: { "line-color": "#d6336c", "line-width": 3 },
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

function setData(map: MapLibreMap, id: string, data: GeoJSONData) {
  (map.getSource(id) as GeoJSONSource | undefined)?.setData(data);
}

export function MapView({
  tracks,
  overview,
  selected,
  showBaseMap,
  showAll,
  cursor,
  highlight,
  focus,
  version,
  onSelect,
}: Props) {
  const container = useRef<HTMLDivElement>(null);
  const mapRef = useRef<MapLibreMap | null>(null);
  const selectedRef = useRef<TrackSummary | null>(null);
  const requestSeq = useRef(0);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const [ready, setReady] = useState(false);

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
      style: showBaseMap ? MAP_STYLE : BLANK_STYLE,
      center: [10, 50],
      zoom: 4,
      attributionControl: { compact: true },
    });
    mapRef.current = map;
    map.on("load", () => {
      addOwnLayers(map);
      setReady(true);
    });
    map.on("moveend", () => refreshDetail.current());
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
      const id = trackAt(e.point);
      if (id != null) onSelectRef.current(id);
    });
    map.on("mousemove", (e) => {
      map.getCanvas().style.cursor = trackAt(e.point) != null ? "pointer" : "";
    });
    return () => map.remove();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !ready) return;
    // transformStyle carries our sources and layers over to the new base style.
    map.setStyle(showBaseMap ? MAP_STYLE : BLANK_STYLE, {
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showBaseMap]);

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

  useEffect(() => {
    selectedRef.current = selected;
    const map = mapRef.current;
    if (!map) return;
    const b = selected?.bbox;
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
    setData(map, "highlight", {
      type: "FeatureCollection",
      features: highlight.length
        ? [{ type: "Feature", properties: {}, geometry: { type: "MultiPoint", coordinates: highlight } }]
        : [],
    });
  }, [highlight, ready]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !focus) return;
    const [x0, y0, x1, y1] = focus.bbox;
    map.fitBounds(
      [
        [x0, y0],
        [x1, y1],
      ],
      { padding: 80, maxZoom: 18, duration: 500 },
    );
  }, [focus]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !map.getSource("cursor")) return;
    setData(map, "cursor", {
      type: "FeatureCollection",
      features: cursor ? [{ type: "Feature", properties: {}, geometry: { type: "Point", coordinates: cursor } }] : [],
    });
  }, [cursor]);

  return <div ref={container} className="map" />;
}
