use std::borrow::Cow;

use anyhow::{Result, bail};
use chrono::{DateTime, NaiveDateTime};
use quick_xml::Reader;
use quick_xml::events::{BytesRef, BytesStart, Event};

use crate::model::{ParsedFile, ParsedTrack, Points, Waypoint};

#[derive(Clone, Copy, PartialEq)]
enum Capture {
    None,
    Name,
    Desc,
    Type,
    Ele,
    Time,
    Speed,
    Course,
    WptName,
    WptDesc,
    WptCmt,
    WptSym,
}

/// Decodes the file as UTF-8, UTF-16 (with BOM) or, for old loggers that wrote ISO-8859-1
/// without declaring it, Latin-1.
pub(crate) fn decode(bytes: &[u8]) -> Cow<'_, str> {
    let utf16 = |be: bool| {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
            .collect();
        Cow::Owned(String::from_utf16_lossy(&units))
    };
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return utf16(false);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return utf16(true);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Cow::Borrowed(s),
        // Cut off inside a multibyte character: the file is UTF-8 up to the cut.
        Err(e) if e.error_len().is_none() => Cow::Borrowed(std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap()),
        Err(_) => Cow::Owned(bytes.iter().map(|&b| b as char).collect()),
    }
}

/// Streaming parse: memory stays proportional to the point count, never to a DOM.
/// Every `<trk>` and `<rte>` becomes one track; `<wpt>` become waypoints. Values are only
/// taken from direct children of their element, so nested `<extensions>` cannot override
/// them. A file that breaks off (logger lost power) keeps everything read before the break.
pub fn parse(bytes: &[u8]) -> Result<ParsedFile> {
    let text = decode(bytes);
    let mut reader = Reader::from_str(&text);

    let mut tracks = Vec::new();
    let mut waypoints = Vec::new();
    let mut current: Option<ParsedTrack> = None;
    let mut wpt: Option<Waypoint> = None;
    let mut wpt_cmt: Option<String> = None;
    let mut depth = 0usize;
    let mut trk_depth: Option<usize> = None;
    let mut pt_depth: Option<usize> = None;
    let mut wpt_depth: Option<usize> = None;
    let mut pt_lat = f64::NAN;
    let mut pt_lon = f64::NAN;
    let mut pt_ele: Option<f32> = None;
    let mut pt_time: Option<i64> = None;
    let mut pt_speed: Option<f32> = None;
    let mut pt_course: Option<f32> = None;
    let mut capture = Capture::None;
    let mut capture_depth = 0usize;
    let mut buf = String::new();

    loop {
        let event = match reader.read_event() {
            Ok(e) => e,
            Err(e) => {
                let nothing_read = tracks.is_empty()
                    && waypoints.is_empty()
                    && current.as_ref().is_none_or(|t: &ParsedTrack| t.points.is_empty());
                if nothing_read {
                    bail!("XML error at byte {}: {e}", reader.buffer_position());
                }
                break;
            }
        };
        match event {
            Event::Start(e) => {
                depth += 1;
                let parent = depth - 1;
                match e.local_name().as_ref() {
                    "trk" | "rte" if trk_depth.is_none() && wpt_depth.is_none() => {
                        current = Some(ParsedTrack {
                            name: None,
                            description: None,
                            activity: None,
                            points: Points::default(),
                        });
                        trk_depth = Some(depth);
                    }
                    "trkseg" if trk_depth == Some(parent) => {
                        if let Some(t) = current.as_mut() {
                            t.points.start_segment();
                        }
                    }
                    "trkpt" | "rtept" if current.is_some() && pt_depth.is_none() => {
                        (pt_lat, pt_lon) = lat_lon(&e);
                        pt_ele = None;
                        pt_time = None;
                        pt_speed = None;
                        pt_course = None;
                        pt_depth = Some(depth);
                    }
                    "wpt" if trk_depth.is_none() && wpt_depth.is_none() => {
                        wpt = Some(new_waypoint(&e));
                        wpt_cmt = None;
                        pt_ele = None;
                        pt_time = None;
                        wpt_depth = Some(depth);
                    }
                    tag if capture == Capture::None => {
                        let what = if pt_depth == Some(parent) {
                            match tag {
                                "ele" => Capture::Ele,
                                "time" => Capture::Time,
                                "speed" => Capture::Speed,
                                "course" => Capture::Course,
                                _ => Capture::None,
                            }
                        } else if pt_depth.is_some_and(|d| d < parent) {
                            // GPX 1.1 has no speed or course; writers put them in <extensions>.
                            match tag {
                                "speed" => Capture::Speed,
                                "course" => Capture::Course,
                                _ => Capture::None,
                            }
                        } else if wpt_depth == Some(parent) {
                            match tag {
                                "name" => Capture::WptName,
                                "desc" => Capture::WptDesc,
                                "cmt" => Capture::WptCmt,
                                "sym" => Capture::WptSym,
                                "ele" => Capture::Ele,
                                "time" => Capture::Time,
                                _ => Capture::None,
                            }
                        } else if trk_depth == Some(parent) {
                            match tag {
                                "name" => Capture::Name,
                                "desc" => Capture::Desc,
                                "type" => Capture::Type,
                                _ => Capture::None,
                            }
                        } else {
                            Capture::None
                        };
                        if what != Capture::None {
                            capture = what;
                            capture_depth = depth;
                            buf.clear();
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => match e.local_name().as_ref() {
                "wpt" if trk_depth.is_none() && wpt_depth.is_none() => {
                    push_waypoint(&mut waypoints, new_waypoint(&e), None, None, None)
                }
                "trkpt" | "rtept" if pt_depth.is_none() => {
                    if let Some(t) = current.as_mut() {
                        let (lat, lon) = lat_lon(&e);
                        push_point(&mut t.points, lat, lon, None, None, None, None);
                    }
                }
                _ => {}
            },
            Event::Text(t) if capture != Capture::None => buf.push_str(&t.xml10_content()),
            Event::CData(t) if capture != Capture::None => buf.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if capture != Capture::None => push_ref(&mut buf, &r),
            Event::End(_) => {
                if capture != Capture::None && depth == capture_depth {
                    let value = buf.trim();
                    match capture {
                        Capture::Name => set_text(current.as_mut().map(|t| &mut t.name), value),
                        Capture::Desc => set_text(current.as_mut().map(|t| &mut t.description), value),
                        Capture::Type => {
                            if let (Some(t), Some(a)) = (current.as_mut(), crate::activity::normalize(value)) {
                                t.activity = Some(a);
                            }
                        }
                        Capture::Ele => pt_ele = value.parse::<f32>().ok().filter(|v| v.is_finite()),
                        Capture::Time => pt_time = parse_time(value),
                        // The first one wins: some loggers repeat it rounded in <extensions>.
                        Capture::Speed => pt_speed = pt_speed.or_else(|| value.parse().ok()),
                        Capture::Course => pt_course = pt_course.or_else(|| value.parse().ok()),
                        Capture::WptName => set_text(wpt.as_mut().map(|w| &mut w.name), value),
                        Capture::WptDesc => set_text(wpt.as_mut().map(|w| &mut w.description), value),
                        Capture::WptCmt => set_text(Some(&mut wpt_cmt), value),
                        Capture::WptSym => set_text(wpt.as_mut().map(|w| &mut w.symbol), value),
                        Capture::None => {}
                    }
                    capture = Capture::None;
                } else if pt_depth == Some(depth) {
                    pt_depth = None;
                    if let Some(t) = current.as_mut() {
                        push_point(&mut t.points, pt_lat, pt_lon, pt_ele, pt_time, pt_speed, pt_course);
                    }
                } else if wpt_depth == Some(depth) {
                    wpt_depth = None;
                    if let Some(w) = wpt.take() {
                        push_waypoint(&mut waypoints, w, wpt_cmt.take(), pt_ele, pt_time);
                    }
                } else if trk_depth == Some(depth) {
                    trk_depth = None;
                    if let Some(t) = current.take() {
                        tracks.push(t);
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // A cut-off file ends inside an open point or track; keep what is complete.
    if pt_depth.is_some() {
        if let Some(t) = current.as_mut() {
            if pt_time.is_some() || pt_ele.is_some() {
                push_point(&mut t.points, pt_lat, pt_lon, pt_ele, pt_time, pt_speed, pt_course);
            }
        }
    }
    if let Some(t) = current.take() {
        tracks.push(t);
    }
    Ok(ParsedFile { tracks, waypoints })
}

/// Appends the text of an entity or character reference.
pub(crate) fn push_ref(buf: &mut String, r: &BytesRef) {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        buf.push(c);
    } else {
        buf.push_str(match &**r {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            _ => "",
        });
    }
}

fn new_waypoint(e: &BytesStart) -> Waypoint {
    let (lat, lon) = lat_lon(e);
    Waypoint { name: None, description: None, symbol: None, kind: "waypoint", lat, lon, ele: None, time: None }
}

/// `<desc>` wins over `<cmt>`; converters often repeat the name in both.
fn push_waypoint(out: &mut Vec<Waypoint>, mut w: Waypoint, cmt: Option<String>, ele: Option<f32>, time: Option<i64>) {
    if !(w.lat.is_finite() && w.lon.is_finite() && w.lat.abs() <= 90.0 && w.lon.abs() <= 180.0) {
        return;
    }
    if w.description.is_none() {
        w.description = cmt;
    }
    if w.description.as_deref().is_some_and(|d| Some(d) == w.name.as_deref()) {
        w.description = None;
    }
    w.kind = Waypoint::kind_for_name(w.name.as_deref());
    w.ele = ele;
    w.time = time;
    out.push(w);
}

fn set_text(target: Option<&mut Option<String>>, value: &str) {
    if let Some(slot) = target {
        if !value.is_empty() {
            *slot = Some(value.to_string());
        }
    }
}

fn push_point(
    points: &mut Points,
    lat: f64,
    lon: f64,
    ele: Option<f32>,
    time: Option<i64>,
    speed: Option<f32>,
    course: Option<f32>,
) {
    if lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 {
        points.push_recorded(lat, lon, ele, time, speed, course);
    }
}

fn lat_lon(e: &BytesStart) -> (f64, f64) {
    let mut lat = f64::NAN;
    let mut lon = f64::NAN;
    for attr in e.attributes().with_checks(false).flatten() {
        let v = attr.value.trim().parse::<f64>().ok();
        match attr.key.local_name().as_ref() {
            "lat" => lat = v.unwrap_or(f64::NAN),
            "lon" => lon = v.unwrap_or(f64::NAN),
            _ => {}
        }
    }
    (lat, lon)
}

pub fn parse_time(s: &str) -> Option<i64> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    // Offsets without a colon, e.g. "+0200".
    if let Ok(dt) = DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%z") {
        return Some(dt.timestamp_millis());
    }
    // Some writers omit the zone; GPX mandates UTC, so assume it.
    let s = s.trim_end_matches('Z');
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f"))
        .ok()
        .map(|dt| dt.and_utc().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<gpx version="1.1" xmlns="http://www.topografix.com/GPX/1/1">
  <wpt lat="1" lon="1"><name>LAP001</name><cmt>LAP001</cmt><desc>LAP001</desc></wpt>
  <wpt lat="2" lon="3"><ele>12</ele><name><![CDATA[Point 7]]></name><sym>Flag</sym></wpt>
  <trk><name>Morning &amp; walk</name><type>Walking</type><desc><![CDATA[Nice]]></desc>
    <trkseg>
      <trkpt lat="52.5" lon="13.4"><ele>34.5</ele><time>2024-05-15T17:26:38Z</time><name>pt</name></trkpt>
      <trkpt lat="52.6" lon="13.5"/>
    </trkseg>
    <trkseg><trkpt lat="52.7" lon="13.6"><time>2024-05-15T17:30:00</time></trkpt></trkseg>
  </trk>
  <trk><trkseg><trkpt lat="1" lon="2"></trkpt></trkseg></trk>
</gpx>"#;

    #[test]
    fn parses_tracks_segments_and_fields() {
        let file = parse(SAMPLE.as_bytes()).unwrap();
        let tracks = &file.tracks;
        assert_eq!(tracks.len(), 2);
        assert_eq!(file.waypoints.len(), 2);
        assert_eq!((file.waypoints[0].kind, file.waypoints[0].description.as_deref()), ("lap", None));
        let w = &file.waypoints[1];
        assert_eq!((w.name.as_deref(), w.symbol.as_deref(), w.ele, w.kind), (Some("Point 7"), Some("Flag"), Some(12.0), "waypoint"));
        let t = &tracks[0];
        assert_eq!(t.name.as_deref(), Some("Morning & walk"));
        assert_eq!(t.description.as_deref(), Some("Nice"));
        assert_eq!(t.activity.as_deref(), Some("walking"));
        assert_eq!(t.points.len(), 3);
        assert_eq!(t.points.segments(), vec![(0, 2), (2, 3)]);
        assert_eq!(t.points.ele[0], 34.5);
        assert!(t.points.ele[1].is_nan());
        assert_eq!(t.points.time[0], 1715793998000);
        assert_eq!(t.points.time[2], 1715794200000);
        assert_eq!(tracks[1].points.len(), 1);
    }

    #[test]
    fn reads_recorded_speed_and_course_of_gpx_1_0_and_extensions() {
        let file = parse(
            br#"<gpx><trk><trkseg>
              <trkpt lat="1" lon="2"><speed>0.19</speed><course>370</course>
                <extensions><speed>0.2</speed></extensions></trkpt>
              <trkpt lat="1" lon="2"><extensions><gpxtpx:TrackPointExtension>
                <gpxtpx:speed>3.5</gpxtpx:speed><gpxtpx:course>90</gpxtpx:course>
              </gpxtpx:TrackPointExtension></extensions></trkpt>
              <trkpt lat="1" lon="2"/>
            </trkseg></trk></gpx>"#,
        )
        .unwrap();
        let p = &file.tracks[0].points;
        assert_eq!((p.speed[0], p.course[0]), (0.19, 10.0), "the first speed wins; course wraps");
        assert_eq!((p.speed[1], p.course[1]), (3.5, 90.0));
        assert!(p.speed[2].is_nan() && p.course[2].is_nan());
    }

    #[test]
    fn tolerates_latin1_and_truncation() {
        let mut bytes = b"<gpx><trk><name>M\xfcnchen</name><trkseg><trkpt lat=\"1\" lon=\"2\"/>".to_vec();
        bytes.extend_from_slice(b"<trkpt lat=\"3\" lon=\"4\">");
        let tracks = parse(&bytes).unwrap().tracks;
        assert_eq!(tracks[0].name.as_deref(), Some("München"));
        assert_eq!(tracks[0].points.len(), 1);
    }

    #[test]
    fn ignores_values_nested_in_extensions() {
        let gpx = r#"<gpx><wpt lat="1" lon="2"><time>2024-01-01T00:00:00Z</time><cmt>short</cmt><desc>long one</desc>
            <extensions><x:a><time>1999-01-01T00:00:00Z</time></x:a></extensions></wpt>
            <trk><name>Real</name><type>running</type><extensions><x:foo><name>Ext</name><type>text/html</type></x:foo></extensions>
            <trkseg><trkpt lat="1" lon="2"><time>2024-01-01T00:00:00Z</time><extensions><x:t><time>1999-01-01T00:00:00Z</time></x:t></extensions></trkpt></trkseg></trk></gpx>"#;
        let f = parse(gpx.as_bytes()).unwrap();
        let t = &f.tracks[0];
        assert_eq!((t.name.as_deref(), t.activity.as_deref()), (Some("Real"), Some("running")));
        assert_eq!(t.points.time[0], 1704067200000);
        let w = &f.waypoints[0];
        assert_eq!((w.description.as_deref(), w.time), (Some("long one"), Some(1704067200000)));
    }

    #[test]
    fn keeps_points_before_a_break_inside_a_tag() {
        let gpx = b"<gpx><trk><trkseg><trkpt lat=\"1\" lon=\"2\"/><trkpt lat=\"3\" lon=\"4\"/><trkpt lat=\"5\" lo";
        let f = parse(gpx).unwrap();
        assert_eq!(f.tracks[0].points.len(), 2);
        assert!(parse(b"<gpx><trk><trkseg><trkpt la").is_err(), "nothing readable is still an error");
    }

    #[test]
    fn non_ascii_waypoint_names_and_encodings() {
        let f = parse("<gpx><wpt lat=\"1\" lon=\"2\"><name>Brücke</name></wpt></gpx>".as_bytes()).unwrap();
        assert_eq!((f.waypoints[0].name.as_deref(), f.waypoints[0].kind), (Some("Brücke"), "waypoint"));

        // UTF-8 cut inside "ü" must not turn the rest into Latin-1.
        let mut cut = "<gpx><trk><name>München</name><trkseg><trkpt lat=\"1\" lon=\"2\"/></trkseg></trk><wpt><name>ü".as_bytes().to_vec();
        cut.pop();
        assert_eq!(parse(&cut).unwrap().tracks[0].name.as_deref(), Some("München"));

        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("<gpx><trk><name>Zürich</name><trkseg><trkpt lat=\"1\" lon=\"2\"/></trkseg></trk></gpx>".encode_utf16().flat_map(|u| u.to_le_bytes()))
            .collect();
        assert_eq!(parse(&utf16).unwrap().tracks[0].name.as_deref(), Some("Zürich"));
    }

    #[test]
    fn parses_offsets_without_colon() {
        assert_eq!(parse_time("2024-05-15T19:26:38+0200"), Some(1715793998000));
    }
}
