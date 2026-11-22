use anyhow::{Result, bail};
use chrono::{Local, NaiveDateTime};
use quick_xml::Reader;
use quick_xml::events::Event;

use super::gpx::{decode, parse_time, push_ref};
use crate::model::{NO_TIME, ParsedFile, ParsedTrack, Points, Waypoint};

type Coord = (f64, f64, Option<f32>);

#[derive(Clone, Copy, PartialEq)]
enum Geom {
    None,
    Point,
    Line,
    Track,
}

#[derive(Clone, Copy, PartialEq)]
enum Capture {
    None,
    Name,
    Desc,
    Coordinates,
    When,
    GxCoord,
}

#[derive(Default)]
struct Segment {
    coords: Vec<Coord>,
    times: Vec<Option<i64>>,
}

#[derive(Default)]
struct Placemark {
    depth: usize,
    name: Option<String>,
    description: Option<String>,
    when: Option<i64>,
    segments: Vec<Segment>,
    points: Vec<Coord>,
}

/// Every Placemark with lines (`LineString`, `gx:Track`, also inside `MultiGeometry` or
/// `gx:MultiTrack`) becomes one track with a segment per line; Placemarks with a `Point`
/// become waypoints. Polygons are areas, not tracks, and are skipped.
pub fn parse(bytes: &[u8]) -> Result<ParsedFile> {
    let text = decode(bytes);
    let mut reader = Reader::from_str(&text);

    let mut placemarks: Vec<Placemark> = Vec::new();
    let mut pm: Option<Placemark> = None;
    let mut depth = 0usize;
    let mut geom = Geom::None;
    let mut geom_depth = 0usize;
    let mut polygon_depth: Option<usize> = None;
    let mut timestamp_depth: Option<usize> = None;
    let mut capture = Capture::None;
    let mut capture_depth = 0usize;
    let mut buf = String::new();

    loop {
        let event = match reader.read_event() {
            Ok(e) => e,
            Err(e) => {
                if placemarks.is_empty() && pm.as_ref().is_none_or(|p| p.segments.is_empty() && p.points.is_empty()) {
                    bail!("XML error at byte {}: {e}", reader.buffer_position());
                }
                break;
            }
        };
        match event {
            Event::Start(e) => {
                depth += 1;
                let tag = e.local_name();
                let Some(p) = pm.as_mut() else {
                    if tag.as_ref() == "Placemark" {
                        pm = Some(Placemark { depth, ..Default::default() });
                    }
                    continue;
                };
                if polygon_depth.is_some() {
                    continue;
                }
                match tag.as_ref() {
                    "Polygon" => polygon_depth = Some(depth),
                    "Point" if geom == Geom::None => (geom, geom_depth) = (Geom::Point, depth),
                    "LineString" if geom == Geom::None => (geom, geom_depth) = (Geom::Line, depth),
                    "Track" if geom == Geom::None => {
                        (geom, geom_depth) = (Geom::Track, depth);
                        p.segments.push(Segment::default());
                    }
                    "TimeStamp" if geom == Geom::None => timestamp_depth = Some(depth),
                    tag if capture == Capture::None => {
                        let what = match tag {
                            "name" if depth == p.depth + 1 => Capture::Name,
                            "description" if depth == p.depth + 1 => Capture::Desc,
                            "coordinates" if matches!(geom, Geom::Point | Geom::Line) => Capture::Coordinates,
                            "when" if geom == Geom::Track || timestamp_depth.is_some() => Capture::When,
                            "coord" if geom == Geom::Track => Capture::GxCoord,
                            _ => Capture::None,
                        };
                        if what != Capture::None {
                            (capture, capture_depth) = (what, depth);
                            buf.clear();
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) if capture != Capture::None => buf.push_str(&t.xml10_content()),
            Event::CData(t) if capture != Capture::None => buf.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if capture != Capture::None => push_ref(&mut buf, &r),
            Event::End(_) => {
                if let Some(p) = pm.as_mut() {
                    if capture != Capture::None && depth == capture_depth {
                        let value = buf.trim();
                        match capture {
                            Capture::Name if !value.is_empty() => p.name = Some(value.to_string()),
                            Capture::Desc if !value.is_empty() => p.description = Some(value.to_string()),
                            Capture::Coordinates => {
                                let coords = parse_coordinates(value);
                                if geom == Geom::Point {
                                    p.points.extend(coords.first());
                                } else if !coords.is_empty() {
                                    p.segments.push(Segment { coords, times: Vec::new() });
                                }
                            }
                            Capture::When if geom == Geom::Track => {
                                if let Some(s) = p.segments.last_mut() {
                                    s.times.push(parse_time(value));
                                }
                            }
                            Capture::When => p.when = parse_time(value),
                            Capture::GxCoord => {
                                if let (Some(s), Some(c)) = (p.segments.last_mut(), parse_tuple(value, ' ')) {
                                    s.coords.push(c);
                                }
                            }
                            _ => {}
                        }
                        capture = Capture::None;
                    } else if polygon_depth == Some(depth) {
                        polygon_depth = None;
                    } else if geom != Geom::None && depth == geom_depth {
                        geom = Geom::None;
                    } else if timestamp_depth == Some(depth) {
                        timestamp_depth = None;
                    } else if depth == p.depth {
                        placemarks.extend(pm.take());
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    placemarks.extend(pm.take());

    let mut tracks = Vec::new();
    let mut waypoints = Vec::new();
    for p in placemarks {
        for &(lat, lon, ele) in &p.points {
            waypoints.push(Waypoint {
                name: p.name.clone(),
                description: p.description.clone(),
                symbol: None,
                kind: Waypoint::kind_for_name(p.name.as_deref()),
                lat,
                lon,
                ele,
                time: p.when,
            });
        }
        if p.segments.iter().any(|s| !s.coords.is_empty()) {
            tracks.push(track(p));
        }
    }
    let used = holux_point_times(&mut tracks, &waypoints);
    let waypoints = waypoints.into_iter().zip(used).filter(|(_, u)| !u).map(|(w, _)| w).collect();
    Ok(ParsedFile { tracks, waypoints })
}

fn track(p: Placemark) -> ParsedTrack {
    // Lines clamped to the ground carry 0 for every altitude.
    let flat = p.segments.iter().flat_map(|s| &s.coords).all(|c| c.2.is_none_or(|e| e == 0.0));
    let mut points = Points::default();
    for s in &p.segments {
        points.start_segment();
        let timed = s.times.len() == s.coords.len();
        for (i, &(lat, lon, ele)) in s.coords.iter().enumerate() {
            points.push(lat, lon, if flat { None } else { ele }, if timed { s.times[i] } else { None });
        }
    }
    ParsedTrack { name: p.name, description: p.description, activity: None, points }
}

/// `lon,lat[,alt]` tuples separated by whitespace.
fn parse_coordinates(s: &str) -> Vec<Coord> {
    s.split_ascii_whitespace().filter_map(|t| parse_tuple(t, ',')).collect()
}

fn parse_tuple(t: &str, sep: char) -> Option<Coord> {
    let mut it = t.split(sep).filter(|v| !v.is_empty());
    let lon = it.next()?.trim().parse::<f64>().ok()?;
    let lat = it.next()?.trim().parse::<f64>().ok()?;
    let ele = it.next().and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite());
    (lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some((lat, lon, ele))
}

/// Holux ezTour writes a track's times only as one Point Placemark per track point, named
/// with its local time (`Mon Jul 20 13:36:26 2009`). Those times go onto the matching points
/// of untimed tracks, in order, and the placemarks are returned as used so they do not turn
/// into hundreds of waypoints. The names have no zone; ezTour used the computer's, so the
/// local zone of this computer is the best guess.
fn holux_point_times(tracks: &mut [ParsedTrack], waypoints: &[Waypoint]) -> Vec<bool> {
    let mut used = vec![false; waypoints.len()];
    let stamps: Vec<(usize, i64)> = waypoints
        .iter()
        .enumerate()
        .filter(|(_, w)| w.time.is_none())
        .filter_map(|(i, w)| Some((i, local_ctime(w.name.as_deref()?)?)))
        .collect();
    if stamps.is_empty() {
        return used;
    }
    let mut j = 0;
    for t in tracks.iter_mut().filter(|t| t.points.time.iter().all(|&x| x == NO_TIME)) {
        let p = &mut t.points;
        for i in 0..p.len() {
            let Some(&(w, ms)) = stamps.get(j) else { break };
            let wp = &waypoints[w];
            if (wp.lat - p.lat[i]).abs() < 1e-6 && (wp.lon - p.lon[i]).abs() < 1e-6 {
                p.time[i] = ms;
                used[w] = true;
                j += 1;
            }
        }
    }
    used
}

fn local_ctime(s: &str) -> Option<i64> {
    let dt = NaiveDateTime::parse_from_str(s.trim(), "%a %b %e %H:%M:%S %Y").ok()?;
    Some(dt.and_local_timezone(Local).earliest()?.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lines_multigeometry_points_and_skips_polygons() {
        let kml = r#"<?xml version="1.0"?><kml xmlns="http://www.opengis.net/kml/2.2"><Document><name>Doc</name>
            <Placemark><name>Start</name><TimeStamp><when>2008-01-01T10:00:00Z</when></TimeStamp>
              <Point><coordinates>8.5,50.0,185.8</coordinates></Point></Placemark>
            <Folder><name>Tracks</name>
            <Placemark><name>Flight &amp; back</name><description><![CDATA[<b>x</b>]]></description>
              <MultiGeometry>
                <LineString><altitudeMode>absolute</altitudeMode><coordinates>
                  8.5,50.0,185.8 8.6,50.1,190
                  8.7,50.2</coordinates></LineString>
                <LineString><coordinates>9,51,0 9.1,51.1,0</coordinates></LineString>
              </MultiGeometry></Placemark>
            <Placemark><name>Ground</name><LineString><coordinates>1,2,0 3,4,0</coordinates></LineString></Placemark>
            <Placemark><name>Area</name><Polygon><outerBoundaryIs><LinearRing><coordinates>1,1 2,2 3,1 1,1</coordinates>
              </LinearRing></outerBoundaryIs></Polygon></Placemark>
            </Folder></Document></kml>"#;
        let f = parse(kml.as_bytes()).unwrap();
        assert_eq!(f.tracks.len(), 2);
        let t = &f.tracks[0];
        assert_eq!((t.name.as_deref(), t.description.as_deref()), (Some("Flight & back"), Some("<b>x</b>")));
        assert_eq!(t.points.segments(), vec![(0, 3), (3, 5)]);
        assert_eq!((t.points.lat[1], t.points.lon[1], t.points.ele[1]), (50.1, 8.6, 190.0));
        assert!(t.points.ele[2].is_nan());
        assert!(f.tracks[1].points.ele.iter().all(|e| e.is_nan()), "clamped lines have no altitude");
        assert_eq!(f.waypoints.len(), 1);
        let w = &f.waypoints[0];
        assert_eq!((w.name.as_deref(), w.ele, w.time), (Some("Start"), Some(185.8), Some(1199181600000)));
    }

    #[test]
    fn reads_gx_tracks() {
        let kml = r#"<kml xmlns:gx="http://www.google.com/kml/ext/2.2"><Placemark><name>Run</name><gx:MultiTrack>
            <gx:Track><when>2020-05-01T10:00:00Z</when><when>2020-05-01T10:00:05Z</when>
              <gx:coord>13.4 52.5 34</gx:coord><gx:coord>13.41 52.51 35</gx:coord></gx:Track>
            <gx:Track><when>2020-05-01T11:00:00Z</when><gx:coord>13.5 52.6 30</gx:coord></gx:Track>
            </gx:MultiTrack></Placemark></kml>"#;
        let f = parse(kml.as_bytes()).unwrap();
        let p = &f.tracks[0].points;
        assert_eq!(p.segments(), vec![(0, 2), (2, 3)]);
        assert_eq!((p.lat[1], p.lon[1], p.ele[1]), (52.51, 13.41, 35.0));
        assert_eq!((p.time[0], p.time[1], p.time[2]), (1588327200000, 1588327205000, 1588330800000));
        assert!(f.waypoints.is_empty());
    }

    #[test]
    fn holux_point_placemarks_become_point_times() {
        let point = |name: &str, c: &str| {
            format!("<Placemark><name>\n{name}\n</name><styleUrl>#s</styleUrl><Point><coordinates>\n{c}\n</coordinates></Point></Placemark>")
        };
        let kml = format!(
            "<kml><Document><Placemark><name>Route</name><MultiGeometry><LineString><coordinates>\
             7.7567492,46.3641853,1224.78 7.7577577,46.3643608,1265.97 7.7577577,46.3643608,1265.9 </coordinates>\
             </LineString></MultiGeometry></Placemark><Folder><name>waypoints</name>{}{}{}{}</Folder></Document></kml>",
            point("Mon Jul 20 13:36:26 2009", "7.7567492,46.3641853,1224.78"),
            point("Mon Jul 20 13:36:31 2009", "7.7577577,46.3643608,1265.97"),
            point("Mon Jul 20 13:36:33 2009", "7.7577577,46.3643608,1265.9"),
            point("Summit", "7.8,46.4,2000"),
        );
        let f = parse(kml.as_bytes()).unwrap();
        let p = &f.tracks[0].points;
        let local = |s| local_ctime(s).unwrap();
        assert_eq!(p.time, vec![local("Mon Jul 20 13:36:26 2009"), local("Mon Jul 20 13:36:31 2009"), local("Mon Jul 20 13:36:33 2009")]);
        assert_eq!(f.waypoints.len(), 1);
        assert_eq!(f.waypoints[0].name.as_deref(), Some("Summit"));
    }
}
