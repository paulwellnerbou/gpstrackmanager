use std::fmt::Write;

use crate::catalog::TrackData;

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
