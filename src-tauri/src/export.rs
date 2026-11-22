use std::fmt::Write;

use chrono::{DateTime, SecondsFormat};

use crate::catalog::{TrackData, WaypointRow};
use crate::model::NO_TIME;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// KML with one LineString per segment, for handing a track to Google Earth.
pub fn kml(data: &TrackData, name: &str) -> String {
    let n = data.coords.len();
    let mut out = String::new();
    let _ = write!(
        out,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2"><Document><name>{name}</name>
<Style id="track"><LineStyle><color>ff6c33d6</color><width>3</width></LineStyle></Style>
<Placemark><name>{name}</name><styleUrl>#track</styleUrl><MultiGeometry>
"#,
        name = escape(name)
    );
    for (k, &s) in data.seg_starts.iter().enumerate() {
        let e = data.seg_starts.get(k + 1).map(|&e| e as usize).unwrap_or(n);
        // A single point is no line (invalid KML); it is a glitch the app also leaves out.
        if e < s as usize + 2 {
            continue;
        }
        out.push_str("<LineString><tessellate>1</tessellate><coordinates>\n");
        for i in s as usize..e {
            let (lat, lon) = data.coords[i];
            let _ = writeln!(out, "{lon:.7},{lat:.7}");
        }
        out.push_str("</coordinates></LineString>\n");
    }
    out.push_str("</MultiGeometry></Placemark></Document></kml>\n");
    out
}

pub struct GpxTrack {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Activity key; `activity::normalize` reads it back unchanged.
    pub activity: Option<String>,
    pub data: TrackData,
}

fn gpx_time(ms: i64) -> Option<String> {
    DateTime::from_timestamp_millis(ms).map(|t| t.to_rfc3339_opts(SecondsFormat::AutoSi, true))
}

fn text_element(out: &mut impl std::io::Write, indent: &str, tag: &str, value: Option<&str>) -> std::io::Result<()> {
    match value {
        Some(v) => writeln!(out, "{indent}<{tag}>{}</{tag}>", escape(v)),
        None => Ok(()),
    }
}

/// GPX 1.1 with everything the catalog holds for one file: its waypoints, then one `<trk>`
/// per track with the file's segments (not the glitch splits). Coordinates keep the
/// catalog's 1e-7 degree precision, so a re-import yields the same points.
pub fn gpx(out: &mut impl std::io::Write, tracks: &[GpxTrack], waypoints: &[WaypointRow]) -> std::io::Result<()> {
    writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        out,
        r#"<gpx version="1.1" creator="GPS Track Manager" xmlns="http://www.topografix.com/GPX/1/1" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="http://www.topografix.com/GPX/1/1 http://www.topografix.com/GPX/1/1/gpx.xsd">"#
    )?;
    for w in waypoints {
        writeln!(out, r#"<wpt lat="{:.7}" lon="{:.7}">"#, w.lat, w.lon)?;
        if let Some(e) = w.ele {
            writeln!(out, "  <ele>{e}</ele>")?;
        }
        text_element(out, "  ", "time", w.time.and_then(gpx_time).as_deref())?;
        text_element(out, "  ", "name", w.name.as_deref())?;
        text_element(out, "  ", "desc", w.description.as_deref())?;
        text_element(out, "  ", "sym", w.symbol.as_deref())?;
        writeln!(out, "</wpt>")?;
    }
    for t in tracks {
        writeln!(out, "<trk>")?;
        text_element(out, "  ", "name", t.name.as_deref())?;
        text_element(out, "  ", "desc", t.description.as_deref())?;
        text_element(out, "  ", "type", t.activity.as_deref())?;
        let d = &t.data;
        let n = d.coords.len();
        for (k, &s) in d.file_seg_starts.iter().enumerate() {
            let e = d.file_seg_starts.get(k + 1).map_or(n, |&e| e as usize).min(n);
            if s as usize >= e {
                continue;
            }
            writeln!(out, "<trkseg>")?;
            for i in s as usize..e {
                let (lat, lon) = d.coords[i];
                write!(out, r#"<trkpt lat="{lat:.7}" lon="{lon:.7}">"#)?;
                if !d.ele[i].is_nan() {
                    write!(out, "<ele>{}</ele>", d.ele[i])?;
                }
                if let Some(t) = Some(d.time[i]).filter(|&t| t != NO_TIME).and_then(gpx_time) {
                    write!(out, "<time>{t}</time>")?;
                }
                writeln!(out, "</trkpt>")?;
            }
            writeln!(out, "</trkseg>")?;
        }
        writeln!(out, "</trk>")?;
    }
    writeln!(out, "</gpx>")
}
