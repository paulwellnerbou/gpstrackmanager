import { useEffect, useRef, useState } from "react";
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
  // Photon's extent is [minLon, maxLat, maxLon, minLat].
  const e = p.extent;
  return {
    label,
    detail,
    center: f.geometry.coordinates,
    bbox: e && [e[0], e[3], e[2], e[1]],
    zoom: ZOOM_BY_TYPE[p.type ?? ""] ?? 15,
  };
}

/** "lat, lon" or "lat lon" in decimal degrees. */
function parseCoords(q: string): Place | null {
  const m = q.trim().match(/^(-?\d+(?:\.\d+)?)\s*[,;\s]\s*(-?\d+(?:\.\d+)?)$/);
  if (!m) return null;
  const lat = Number(m[1]);
  const lon = Number(m[2]);
  if (Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
  return { label: `${lat}, ${lon}`, detail: "Coordinates", center: [lon, lat], zoom: 15 };
}

export function GeoSearch({ map }: { map: MapLibreMap | null }) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Place[]>([]);
  const [active, setActive] = useState(0);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pin = useRef<Marker | null>(null);

  useEffect(() => {
    const q = query.trim();
    const coords = parseCoords(q);
    if (coords) {
      setResults([coords]);
      setActive(0);
      setError(null);
      return;
    }
    if (q.length < 2) {
      setResults([]);
      setError(null);
      return;
    }
    const ctrl = new AbortController();
    const timer = setTimeout(async () => {
      const params = new URLSearchParams({ q, limit: "8" });
      const lang = navigator.language.slice(0, 2);
      if (PHOTON_LANGS.includes(lang)) params.set("lang", lang);
      // Bias towards what is on screen, so "Bahnhof" finds the nearby one first.
      const c = map?.getCenter();
      if (c) {
        params.set("lat", c.lat.toFixed(4));
        params.set("lon", c.lng.toFixed(4));
      }
      try {
        const res = await fetch(`${PHOTON}?${params}`, { signal: ctrl.signal });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const json = (await res.json()) as { features: PhotonFeature[] };
        if (ctrl.signal.aborted) return;
        setResults(json.features.map(toPlace));
        setActive(0);
        setError(null);
      } catch (e) {
        if (!ctrl.signal.aborted) setError(`Search failed: ${(e as Error).message}`);
      }
    }, DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      ctrl.abort();
    };
    // The map center is read at request time; it should not retrigger the search.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query]);

  useEffect(() => () => void pin.current?.remove(), []);

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

  const clear = () => {
    setQuery("");
    setResults([]);
    setOpen(false);
    pin.current?.remove();
  };

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setOpen(true);
      const n = results.length;
      if (n) setActive((a) => (a + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
    } else if (e.key === "Enter") {
      // Typed coordinates need no lookup, so don't depend on the list having caught up.
      const p = parseCoords(query) ?? results[Math.min(active, results.length - 1)];
      if (p) go(p);
    } else if (e.key === "Escape") {
      if (open && results.length) setOpen(false);
      else {
        clear();
        e.currentTarget.blur();
      }
    }
  };

  return (
    <div className="geo-search">
      <div className="geo-search-box">
        <input
          type="search"
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
          <button className="geo-search-clear" title="Clear" onMouseDown={(e) => e.preventDefault()} onClick={clear}>
            ×
          </button>
        )}
      </div>
      {open && (results.length > 0 || error) && (
        <ul className="geo-search-results">
          {error && <li className="geo-search-error">{error}</li>}
          {results.map((p, i) => (
            <li
              key={`${p.center[0]},${p.center[1]},${i}`}
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
