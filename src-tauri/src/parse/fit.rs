use anyhow::{Result, anyhow};
use fitparser::de::{DecodeOption, from_bytes_with_options};
use fitparser::profile::MesgNum;
use fitparser::{FitDataRecord, Value};

use crate::model::{ParsedFile, ParsedTrack, Points, Waypoint};

const SEMICIRCLE_TO_DEG: f64 = 180.0 / 2_147_483_648.0;

/// One track per FIT file; a timer stop/start begins a new segment. Laps become lap
/// waypoints at the position where each lap ended.
pub fn parse(bytes: &[u8]) -> Result<ParsedFile> {
    let records = match fitparser::from_bytes(bytes) {
        Ok(r) => r,
        // Devices that lose power mid-write leave bad CRCs but valid records.
        Err(_) => {
            let opts = [DecodeOption::SkipHeaderCrcValidation, DecodeOption::SkipDataCrcValidation]
                .into_iter()
                .collect();
            from_bytes_with_options(bytes, &opts).map_err(|e| anyhow!("FIT decode failed: {e}"))?
        }
    };

    let mut points = Points::default();
    let mut sport: Option<String> = None;
    let mut laps = Vec::new();
    for rec in &records {
        match rec.kind() {
            MesgNum::Record => {
                let lat = field(rec, "position_lat").and_then(num);
                let lon = field(rec, "position_long").and_then(num);
                let (Some(lat), Some(lon)) = (lat, lon) else { continue };
                let (lat, lon) = (lat * SEMICIRCLE_TO_DEG, lon * SEMICIRCLE_TO_DEG);
                if lat.abs() > 90.0 || lon.abs() > 180.0 {
                    continue;
                }
                let ele = field(rec, "enhanced_altitude")
                    .or_else(|| field(rec, "altitude"))
                    .and_then(num)
                    .map(|v| v as f32);
                let time = match field(rec, "timestamp") {
                    Some(Value::Timestamp(t)) => Some(t.timestamp_millis()),
                    _ => None,
                };
                points.push(lat, lon, ele, time);
            }
            MesgNum::Event => {
                let is_timer = matches!(field(rec, "event"), Some(Value::String(s)) if s == "timer");
                let is_start = matches!(field(rec, "event_type"), Some(Value::String(s)) if s == "start");
                if is_timer && is_start && !points.is_empty() {
                    points.start_segment();
                }
            }
            MesgNum::Lap => {
                let pos = |lat: &str, lon: &str| Some((field(rec, lat).and_then(num)?, field(rec, lon).and_then(num)?));
                let Some((lat, lon)) =
                    pos("end_position_lat", "end_position_long").or_else(|| pos("start_position_lat", "start_position_long"))
                else {
                    continue;
                };
                let time = match field(rec, "timestamp") {
                    Some(Value::Timestamp(t)) => Some(t.timestamp_millis()),
                    _ => None,
                };
                laps.push(Waypoint {
                    name: Some(format!("Lap {}", laps.len() + 1)),
                    description: None,
                    symbol: None,
                    kind: "lap",
                    lat: lat * SEMICIRCLE_TO_DEG,
                    lon: lon * SEMICIRCLE_TO_DEG,
                    ele: None,
                    time,
                });
            }
            MesgNum::Session if sport.is_none() => {
                if let Some(Value::String(s)) = field(rec, "sport") {
                    sport = Some(s.clone());
                }
            }
            _ => {}
        }
    }
    let activity = sport.as_deref().and_then(crate::activity::normalize);
    Ok(ParsedFile { tracks: vec![ParsedTrack { name: None, description: None, activity, points }], waypoints: laps })
}

fn field<'a>(rec: &'a FitDataRecord, name: &str) -> Option<&'a Value> {
    rec.fields().iter().find(|f| f.name() == name).map(|f| f.value())
}

fn num(v: &Value) -> Option<f64> {
    match *v {
        Value::SInt8(x) => Some(x as f64),
        Value::UInt8(x) | Value::UInt8z(x) | Value::Byte(x) => Some(x as f64),
        Value::SInt16(x) => Some(x as f64),
        Value::UInt16(x) | Value::UInt16z(x) => Some(x as f64),
        Value::SInt32(x) => Some(x as f64),
        Value::UInt32(x) | Value::UInt32z(x) => Some(x as f64),
        Value::SInt64(x) => Some(x as f64),
        Value::UInt64(x) | Value::UInt64z(x) => Some(x as f64),
        Value::Float32(x) => Some(x as f64),
        Value::Float64(x) => Some(x),
        _ => None,
    }
}
