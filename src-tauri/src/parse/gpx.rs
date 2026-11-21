use std::borrow::Cow;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::model::{ParsedTrack, Points};

#[derive(Clone, Copy, PartialEq)]
enum Capture {
    None,
    Name,
    Desc,
    Ele,
    Time,
}

/// Streaming parse: memory stays proportional to the point count, never to a DOM.
/// Every `<trk>` and `<rte>` becomes one track.
pub fn parse(bytes: &[u8]) -> Result<Vec<ParsedTrack>> {
    // Old loggers wrote ISO-8859-1 without declaring it; Latin-1 maps bytes 1:1 to chars.
    let text: Cow<str> = match std::str::from_utf8(bytes) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => Cow::Owned(bytes.iter().map(|&b| b as char).collect()),
    };
    let mut reader = Reader::from_str(&text);

    let mut tracks = Vec::new();
    let mut current: Option<ParsedTrack> = None;
    let mut in_point = false;
    let mut pt_lat = f64::NAN;
    let mut pt_lon = f64::NAN;
    let mut pt_ele: Option<f32> = None;
    let mut pt_time: Option<i64> = None;
    let mut capture = Capture::None;
    let mut buf = String::new();

    loop {
        let event = reader
            .read_event()
            .with_context(|| format!("XML error at byte {}", reader.buffer_position()))?;
        match event {
            Event::Start(e) => match e.local_name().as_ref() {
                "trk" | "rte" => {
                    current = Some(ParsedTrack { name: None, description: None, points: Points::default() });
                }
                "trkseg" => {
                    if let Some(t) = current.as_mut() {
                        t.points.start_segment();
                    }
                }
                "trkpt" | "rtept" if current.is_some() => {
                    (pt_lat, pt_lon) = lat_lon(&e);
                    pt_ele = None;
                    pt_time = None;
                    in_point = true;
                }
                "name" if current.is_some() && !in_point => begin(&mut capture, &mut buf, Capture::Name),
                "desc" if current.is_some() && !in_point => begin(&mut capture, &mut buf, Capture::Desc),
                "ele" if in_point => begin(&mut capture, &mut buf, Capture::Ele),
                "time" if in_point => begin(&mut capture, &mut buf, Capture::Time),
                _ => {}
            },
            Event::Empty(e) => {
                if matches!(e.local_name().as_ref(), "trkpt" | "rtept") {
                    if let Some(t) = current.as_mut() {
                        let (lat, lon) = lat_lon(&e);
                        push_point(&mut t.points, lat, lon, None, None);
                    }
                }
            }
            Event::Text(t) if capture != Capture::None => buf.push_str(&t.xml10_content()),
            Event::CData(t) if capture != Capture::None => buf.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if capture != Capture::None => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    buf.push(c);
                } else {
                    buf.push_str(match &*r {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "",
                    });
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "trk" | "rte" => {
                    if let Some(t) = current.take() {
                        tracks.push(t);
                    }
                }
                "trkpt" | "rtept" if in_point => {
                    in_point = false;
                    if let Some(t) = current.as_mut() {
                        push_point(&mut t.points, pt_lat, pt_lon, pt_ele, pt_time);
                    }
                }
                "name" | "desc" | "ele" | "time" if capture != Capture::None => {
                    let value = buf.trim();
                    match capture {
                        Capture::Name => set_text(current.as_mut().map(|t| &mut t.name), value),
                        Capture::Desc => set_text(current.as_mut().map(|t| &mut t.description), value),
                        Capture::Ele => pt_ele = value.parse::<f32>().ok().filter(|v| v.is_finite()),
                        Capture::Time => pt_time = parse_time(value),
                        Capture::None => {}
                    }
                    capture = Capture::None;
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    // Truncated files (logger lost power) still yield the points read so far.
    if let Some(t) = current.take() {
        tracks.push(t);
    }
    Ok(tracks)
}

fn begin(capture: &mut Capture, buf: &mut String, what: Capture) {
    *capture = what;
    buf.clear();
}

fn set_text(target: Option<&mut Option<String>>, value: &str) {
    if let Some(slot) = target {
        if !value.is_empty() {
            *slot = Some(value.to_string());
        }
    }
}

fn push_point(points: &mut Points, lat: f64, lon: f64, ele: Option<f32>, time: Option<i64>) {
    if lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 {
        points.push(lat, lon, ele, time);
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
  <wpt lat="1" lon="1"><name>ignored waypoint</name></wpt>
  <trk><name>Morning &amp; walk</name><desc><![CDATA[Nice]]></desc>
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
        let tracks = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(tracks.len(), 2);
        let t = &tracks[0];
        assert_eq!(t.name.as_deref(), Some("Morning & walk"));
        assert_eq!(t.description.as_deref(), Some("Nice"));
        assert_eq!(t.points.len(), 3);
        assert_eq!(t.points.segments(), vec![(0, 2), (2, 3)]);
        assert_eq!(t.points.ele[0], 34.5);
        assert!(t.points.ele[1].is_nan());
        assert_eq!(t.points.time[0], 1715793998000);
        assert_eq!(t.points.time[2], 1715794200000);
        assert_eq!(tracks[1].points.len(), 1);
    }

    #[test]
    fn tolerates_latin1_and_truncation() {
        let mut bytes = b"<gpx><trk><name>M\xfcnchen</name><trkseg><trkpt lat=\"1\" lon=\"2\"/>".to_vec();
        bytes.extend_from_slice(b"<trkpt lat=\"3\" lon=\"4\">");
        let tracks = parse(&bytes).unwrap();
        assert_eq!(tracks[0].name.as_deref(), Some("München"));
        assert_eq!(tracks[0].points.len(), 1);
    }
}
