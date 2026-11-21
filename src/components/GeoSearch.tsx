import { useEffect, useId, useRef, useState } from "react";
import { Marker, type Map as MapLibreMap } from "maplibre-gl";

// Photon (OSM data, run by komoot) allows search-as-you-type without a key;
// Nominatim's usage policy forbids autocomplete.
const PHOTON = "https://photon.komoot.io/api/";
const PHOTON_LANGS = ["de", "en", "fr"];
const DEBOUNCE_MS = 300;

interface Place {
  label: string;
  detail: string;
  center: [number, number];
  /** [west, south, east, north] */
  bbox?: [number, number, number, number];
  zoom: number;
}

interface PhotonFeature {
  geometry: { coordinates: [number, number] };
  properties: Record<string, string | undefined> & { extent?: [number, number, number, number] };
}

/** Results and the trimmed query they answer, so a list for older text is never mistaken for the current one. */
interface ResultList {
  q: string;
  places: Place[];
  error?: string;
}

const ZOOM_BY_TYPE: Record<string, number> = {
  house: 18,
  street: 16,
  locality: 14,
  district: 13,
  city: 12,
  county: 9,
  state: 7,
  country: 5,
};

function toPlace(f: PhotonFeature): Place {
  const p = f.properties;
  const street = p.street && [p.street, p.housenumber].filter(Boolean).join(" ");
  const label = p.name ?? street ?? p.city ?? p.country ?? "?";
  const detail = [street !== label ? street : undefined, p.postcode, p.city !== label ? p.city : undefined, p.state, p.country]
    .filter(Boolean)
    .join(", ");
  // Photon's extent is [minLon, maxLat, maxLon, minLat]. Areas crossing the antimeridian
  // (Fiji, Russia, Kiribati) come back spanning nearly all longitudes, which would fit the
  // whole world, so those fly to the center instead.
  const e = p.extent;
  const wraps = e != null && e[2] - e[0] > 300;
  const zoom = ZOOM_BY_TYPE[p.type ?? ""] ?? 15;
  return {
    label,
    detail,
    center: f.geometry.coordinates,
    bbox: e && !wraps ? [e[0], e[3], e[2], e[1]] : undefined,
    zoom: wraps ? Math.min(zoom, 4) : zoom,
  };
}

const NUM_DOT = String.raw`-?\d+(?:\.\d+)?`;
const NUM_COMMA = String.raw`-?\d+(?:,\d+)?`;
const COORDS = [
  // "47.42, 10.98", "47.42 10.98", "47.42;10.98"
  new RegExp(`^(${NUM_DOT})\\s*[,;\\s]\\s*(${NUM_DOT})$`),
  // Decimal commas ("47,42 10,98"): the comma is taken, so only space or ";" can separate.
  new RegExp(`^(${NUM_COMMA})\\s*[;\\s]\\s*(${NUM_COMMA})$`),
];

/** "lat, lon" in decimal degrees, with decimal points or commas. */
function parseCoords(q: string): Place | null {
  const s = q.trim();
  for (const re of COORDS) {
    const m = s.match(re);
    if (!m) continue;
    const lat = Number(m[1].replace(",", "."));
    const lon = Number(m[2].replace(",", "."));
    if (Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
    return { label: `${lat}, ${lon}`, detail: "Coordinates", center: [lon, lat], zoom: 15 };
  }
  return null;
}

export function GeoSearch({ map }: { map: MapLibreMap | null }) {
  const [query, setQuery] = useState("");
  const [list, setList] = useState<ResultList>({ q: "", places: [] });
  const [active, setActive] = useState(0);
  const [open, setOpen] = useState(false);
  const pin = useRef<Marker | null>(null);
  /** Query for which Enter was pressed before its results arrived. */
  const pickOnArrival = useRef<string | null>(null);
  const listId = useId();

  const go = (p: Place) => {
    if (!map) return;
    setOpen(false);
    if (p.bbox) {
      map.fitBounds(
        [
          [p.bbox[0], p.bbox[1]],
          [p.bbox[2], p.bbox[3]],
        ],
        { padding: 60, maxZoom: 17, duration: 800 },
      );
    } else {
      map.flyTo({ center: p.center, zoom: p.zoom, duration: 800 });
    }
    pin.current ??= new Marker({ color: "#2f6fdb" });
    pin.current.setLngLat(p.center).addTo(map);
  };
  const goRef = useRef(go);
  goRef.current = go;

  useEffect(() => {
    const q = query.trim();
    if (pickOnArrival.current !== q) pickOnArrival.current = null;
    const coords = parseCoords(q);
    if (coords) {
      setList({ q, places: [coords] });
      setActive(0);
      return;
    }
    if (q.length < 2) {
      setList({ q, places: [] });
      return;
    }
    const ctrl = new AbortController();
    const timer = setTimeout(async () => {
      const params = new URLSearchParams({ q, limit: "8" });
      const lang = navigator.language.slice(0, 2);
      if (PHOTON_LANGS.includes(lang)) params.set("lang", lang);
      // Bias towards what is on screen, so "Bahnhof" finds the nearby one first; the
      // zoom widens the bias radius when the view is wide.
      if (map) {
        const c = map.getCenter();
        params.set("lat", c.lat.toFixed(4));
        params.set("lon", c.lng.toFixed(4));
        params.set("zoom", String(Math.round(map.getZoom())));
      }
      let next: ResultList;
      try {
        const res = await fetch(`${PHOTON}?${params}`, { signal: ctrl.signal });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const json = (await res.json()) as { features: PhotonFeature[] };
        next = { q, places: json.features.map(toPlace) };
      } catch (e) {
        next = { q, places: [], error: `Search failed: ${(e as Error).message}` };
      }
      if (ctrl.signal.aborted) return;
      setList(next);
      setActive(0);
      if (pickOnArrival.current === q) {
        pickOnArrival.current = null;
        if (next.places[0]) goRef.current(next.places[0]);
      }
    }, DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      ctrl.abort();
    };
    // The map view is read at request time; it should not retrigger the search.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query]);

  useEffect(() => () => void pin.current?.remove(), []);

  const clear = () => {
    setQuery("");
    setList({ q: "", places: [] });
    setOpen(false);
    pin.current?.remove();
  };

  const { places, error } = list;
  const showList = open && (places.length > 0 || error != null);

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setOpen(true);
      const n = places.length;
      if (n) setActive((a) => (a + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
    } else if (e.key === "Enter") {
      const q = query.trim();
      const coords = parseCoords(q);
      if (coords) go(coords);
      else if (list.q === q) {
        const p = places[Math.min(active, places.length - 1)];
        if (p) go(p);
      } else if (q.length >= 2) pickOnArrival.current = q;
    } else if (e.key === "Escape") {
      // A search input clears itself on Escape by default; the first Escape only closes the list.
      e.preventDefault();
      if (showList) setOpen(false);
      else {
        clear();
        e.currentTarget.blur();
      }
    }
  };

  const optionId = (i: number) => `${listId}-${i}`;

  return (
    <div className="geo-search">
      <div className="geo-search-box">
        <input
          type="search"
          role="combobox"
          aria-label="Search place"
          aria-autocomplete="list"
          aria-expanded={showList}
          aria-controls={listId}
          aria-activedescendant={showList && places[active] ? optionId(active) : undefined}
          placeholder="Search place or lat, lon"
          value={query}
          spellCheck={false}
          onChange={(e) => {
            setQuery(e.target.value);
            setOpen(true);
          }}
          onFocus={() => setOpen(true)}
          onBlur={() => setOpen(false)}
          onKeyDown={onKey}
        />
        {query && (
          <button
            className="geo-search-clear"
            title="Clear"
            aria-label="Clear search"
            onMouseDown={(e) => e.preventDefault()}
            onClick={clear}
          >
            ×
          </button>
        )}
      </div>
      {showList && (
        <ul className="geo-search-results" id={listId} role="listbox">
          {error && (
            <li className="geo-search-error" role="alert">
              {error}
            </li>
          )}
          {places.map((p, i) => (
            <li
              key={`${p.center[0]},${p.center[1]},${i}`}
              id={optionId(i)}
              role="option"
              aria-selected={i === active}
              className={i === active ? "active" : undefined}
              // mousedown, not click: the input's blur would close the list first.
              onMouseDown={(e) => {
                e.preventDefault();
                go(p);
              }}
              onMouseEnter={() => setActive(i)}
            >
              <span className="geo-search-label">{p.label}</span>
              {p.detail && <span className="geo-search-detail">{p.detail}</span>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
