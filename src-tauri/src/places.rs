//! Offline reverse geocoding against GeoNames places with at least 1000 inhabitants
//! (CC BY 4.0, built by `examples/build_places.rs`). Nothing leaves the machine.

use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

use serde::Serialize;

use crate::geo::haversine_m;

static DATA: &[u8] = include_bytes!("../data/places.tsv.gz");

/// Bump when the place data or the matching rules change; stored places are then recomputed.
pub const PLACES_VERSION: i64 = 2;

const CELL_DEG: f64 = 0.25;
const LON_CELLS: i32 = (360.0 / CELL_DEG) as i32;
const KM_PER_DEG: f64 = 111.195;
/// GeoNames has centre points only, so a place's extent is estimated from its population
/// (about 3000 inhabitants per km²), capped because megacities are not round.
const RADIUS_KM_PER_SQRT_POP: f64 = 0.0103;
const MIN_RADIUS_KM: f64 = 1.0;
const MAX_RADIUS_KM: f64 = 25.0;
/// Start and end outside every place are named after the nearest one up to this far away.
const MAX_EDGE_KM: f64 = 35.0;
const GLITCH_M: f64 = 50_000.0;
const SAMPLE_KM: f64 = 1.0;
const MAX_SAMPLES: f64 = 2000.0;

struct Entry {
    lat: f64,
    lon: f64,
    radius_km: f64,
    name: Box<str>,
    region: u32,
}

struct Region {
    name: Option<Box<str>>,
    country: Option<Box<str>>,
}

pub struct PlaceIndex {
    entries: Vec<Entry>,
    regions: Vec<Region>,
    grid: HashMap<(i32, i32), Vec<u32>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Place {
    pub name: String,
    pub region: Option<String>,
    pub country: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Start,
    Via,
    End,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Start => "start",
            Role::Via => "via",
            Role::End => "end",
        }
    }

    pub fn parse(s: &str) -> Role {
        match s {
            "start" => Role::Start,
            "end" => Role::End,
            _ => Role::Via,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrackPlace {
    pub role: Role,
    #[serde(flatten)]
    pub place: Place,
}

fn cell(lat: f64, lon: f64) -> (i32, i32) {
    ((lat / CELL_DEG).floor() as i32, (((lon + 180.0) / CELL_DEG).floor() as i32).rem_euclid(LON_CELLS))
}

fn approx_km(lat0: f64, lon0: f64, lat1: f64, lon1: f64) -> f64 {
    let dlon = (lon1 - lon0 + 540.0) % 360.0 - 180.0;
    let x = dlon * ((lat0 + lat1) / 2.0).to_radians().cos();
    (x * x + (lat1 - lat0).powi(2)).sqrt() * KM_PER_DEG
}

struct Match {
    idx: u32,
    inside: bool,
}

impl PlaceIndex {
    fn parse(tsv: &str) -> PlaceIndex {
        let mut countries: HashMap<&str, &str> = HashMap::new();
        let mut region_ids: HashMap<&str, u32> = HashMap::new();
        let mut regions = Vec::new();
        let mut entries = Vec::new();
        let mut region_id = |key, name: Option<&str>, regions: &mut Vec<Region>, countries: &HashMap<&str, &str>| {
            *region_ids.entry(key).or_insert_with(|| {
                let cc = key.split('.').next().unwrap_or("");
                regions.push(Region { name: name.map(Into::into), country: countries.get(cc).map(|&c| c.into()) });
                regions.len() as u32 - 1
            })
        };
        for line in tsv.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f.as_slice() {
                ["C", cc, name] => {
                    countries.insert(cc, name);
                }
                ["R", key, name] => {
                    region_id(key, Some(name), &mut regions, &countries);
                }
                ["P", name, lat, lon, pop, key] => {
                    let (Ok(lat), Ok(lon)) = (lat.parse(), lon.parse()) else { continue };
                    let pop: f64 = pop.parse().unwrap_or(0.0);
                    entries.push(Entry {
                        lat,
                        lon,
                        radius_km: (RADIUS_KM_PER_SQRT_POP * pop.sqrt()).clamp(MIN_RADIUS_KM, MAX_RADIUS_KM),
                        name: (*name).into(),
                        region: region_id(key, None, &mut regions, &countries),
                    });
                }
                _ => {}
            }
        }
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            grid.entry(cell(e.lat, e.lon)).or_default().push(i as u32);
        }
        PlaceIndex { entries, regions, grid }
    }

    /// Prefers the place the point lies deepest inside, relative to the place's size, so a
    /// suburb wins near its own centre and the city wins elsewhere. Outside every place the
    /// one with the nearest estimated edge is returned.
    fn find(&self, lat: f64, lon: f64) -> Option<Match> {
        let reach = MAX_RADIUS_KM + MAX_EDGE_KM;
        let dlat = reach / KM_PER_DEG;
        let dlon = (dlat / lat.to_radians().cos().max(0.01)).min(180.0);
        let (y0, y1) = (((lat - dlat) / CELL_DEG).floor() as i32, ((lat + dlat) / CELL_DEG).floor() as i32);
        let x0 = ((lon + 180.0 - dlon) / CELL_DEG).floor() as i32;
        let x1 = (((lon + 180.0 + dlon) / CELL_DEG).floor() as i32).min(x0 + LON_CELLS - 1);
        let mut inside: Option<(f64, u32)> = None;
        let mut edge: Option<(f64, u32)> = None;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let Some(ids) = self.grid.get(&(y, x.rem_euclid(LON_CELLS))) else { continue };
                for &i in ids {
                    let e = &self.entries[i as usize];
                    let d = approx_km(lat, lon, e.lat, e.lon);
                    if d < e.radius_km {
                        let depth = d / e.radius_km;
                        if inside.is_none_or(|(b, _)| depth < b) {
                            inside = Some((depth, i));
                        }
                    } else if d - e.radius_km < MAX_EDGE_KM && edge.is_none_or(|(b, _)| d - e.radius_km < b) {
                        edge = Some((d - e.radius_km, i));
                    }
                }
            }
        }
        inside.map(|(_, idx)| Match { idx, inside: true }).or(edge.map(|(_, idx)| Match { idx, inside: false }))
    }

    fn place(&self, idx: u32) -> Place {
        let e = &self.entries[idx as usize];
        let r = &self.regions[e.region as usize];
        Place {
            name: e.name.to_string(),
            region: r.name.as_deref().map(String::from),
            country: r.country.as_deref().map(String::from),
        }
    }

    pub fn lookup(&self, lat: f64, lon: f64) -> Option<Place> {
        self.find(lat, lon).map(|m| self.place(m.idx))
    }

    /// Start and end place plus the places passed through, in order of first appearance.
    /// `coords` may contain NaN pairs as segment breaks.
    pub fn track_places(&self, coords: &[(f64, f64)]) -> Vec<TrackPlace> {
        let runs: Vec<&[(f64, f64)]> = coords.split(|(lat, _)| lat.is_nan()).filter(|r| !r.is_empty()).collect();
        // A point isolated by glitch splitting and far from both neighbours is a bad fix (e.g. a
        // stale one from the previous trip). Sparse loggers also isolate points, but near the rest.
        let far = |p: (f64, f64), q: Option<&(f64, f64)>| q.is_none_or(|q| haversine_m(p.0, p.1, q.0, q.1) > GLITCH_M);
        let lines: Vec<&[(f64, f64)]> = (0..runs.len())
            .filter(|&k| {
                let r = runs[k];
                let prev = k.checked_sub(1).and_then(|j| runs[j].last());
                let next = runs.get(k + 1).and_then(|n| n.first());
                r.len() > 1 || runs.len() == 1 || !(far(r[0], prev) && far(r[0], next))
            })
            .map(|k| runs[k])
            .collect();
        let (Some(first), Some(last)) = (lines.first().map(|r| r[0]), lines.last().and_then(|r| r.last().copied()))
        else {
            return Vec::new();
        };
        let start = self.find(first.0, first.1).map(|m| m.idx);
        let end = self.find(last.0, last.1).map(|m| m.idx);

        let step_km = |a: (f64, f64), b: (f64, f64)| haversine_m(a.0, a.1, b.0, b.1) / 1000.0;
        let total: f64 = lines.iter().flat_map(|r| r.windows(2)).map(|w| step_km(w[0], w[1])).sum();
        let step = SAMPLE_KM.max(total / MAX_SAMPLES);

        let mut via: Vec<u32> = Vec::new();
        for line in &lines {
            let mut since = f64::INFINITY;
            for (i, &p) in line.iter().enumerate() {
                if i > 0 {
                    since += step_km(line[i - 1], p);
                }
                if since < step {
                    continue;
                }
                since = 0.0;
                // Passing near a place is not passing through it, so via places must contain the point.
                if let Some(Match { idx, inside: true }) = self.find(p.0, p.1)
                    && Some(idx) != start
                    && Some(idx) != end
                    && !via.contains(&idx)
                {
                    via.push(idx);
                }
            }
        }

        let row = |role, idx| TrackPlace { role, place: self.place(idx) };
        start
            .map(|i| row(Role::Start, i))
            .into_iter()
            .chain(via.into_iter().map(|i| row(Role::Via, i)))
            .chain(end.map(|i| row(Role::End, i)))
            .collect()
    }
}

/// Loaded on first use (a fraction of a second), then shared by all threads.
pub fn index() -> &'static PlaceIndex {
    static INDEX: OnceLock<PlaceIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut tsv = String::new();
        flate2::read::GzDecoder::new(DATA).read_to_string(&mut tsv).expect("embedded place data is valid gzip");
        PlaceIndex::parse(&tsv)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[TrackPlace], role: Role) -> Vec<&str> {
        v.iter().filter(|p| p.role == role).map(|p| p.place.name.as_str()).collect()
    }

    #[test]
    fn city_centre_and_suburb() {
        let idx = index();
        let berlin = idx.lookup(52.5163, 13.3777).unwrap();
        assert_eq!(berlin.name, "Berlin");
        assert_eq!(berlin.country.as_deref(), Some("Germany"));
        // Inside Berlin's estimated extent, but at the centre of a separate municipality.
        assert_eq!(idx.lookup(52.4085, 13.2270).unwrap().name, "Kleinmachnow");
    }

    #[test]
    fn open_sea_has_no_place() {
        assert!(index().lookup(45.0, -30.0).is_none());
    }

    #[test]
    fn route_between_two_cities() {
        let (a, b) = ((52.5163, 13.3777), (52.3989, 13.0657));
        let coords: Vec<(f64, f64)> =
            (0..=100).map(|i| i as f64 / 100.0).map(|f| (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)).collect();
        let places = index().track_places(&coords);
        assert_eq!(names(&places, Role::Start), ["Berlin"]);
        assert_eq!(names(&places, Role::End), ["Potsdam"]);
    }

    #[test]
    fn isolated_glitch_points_are_ignored() {
        let thun = (46.7580, 7.6280);
        let walk: Vec<(f64, f64)> = (0..20).map(|i| (52.5163 + i as f64 * 0.0005, 13.3777)).collect();
        let mut coords = vec![thun, (f64::NAN, f64::NAN)];
        coords.extend(&walk);
        coords.extend([(f64::NAN, f64::NAN), thun]);
        let places = index().track_places(&coords);
        assert_eq!(names(&places, Role::Start), ["Berlin"]);
        assert_eq!(names(&places, Role::End), ["Berlin"]);
        assert!(names(&places, Role::Via).is_empty());
        // A track that is nothing but single points still gets a place.
        assert_eq!(names(&index().track_places(&[thun]), Role::Start), ["Thun"]);
    }

    #[test]
    fn sparse_points_near_the_track_count() {
        // A recording gap isolates the last fix, but it is close to the rest: Berlin to Potsdam.
        let walk: Vec<(f64, f64)> = (0..20).map(|i| (52.5163 + i as f64 * 0.0005, 13.3777)).collect();
        let mut coords = walk.clone();
        coords.extend([(f64::NAN, f64::NAN), (52.3989, 13.0657)]);
        assert_eq!(names(&index().track_places(&coords), Role::End), ["Potsdam"]);
    }
}
