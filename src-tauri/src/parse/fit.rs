use anyhow::{Result, anyhow};
use fitparser::de::{DecodeOption, FitObject, FitStreamProcessor};
use fitparser::profile::MesgNum;
use fitparser::{FitDataRecord, Value};

use crate::model::{ParsedFile, ParsedTrack, Points, Waypoint};

const SEMICIRCLE_TO_DEG: f64 = 180.0 / 2_147_483_648.0;

/// One track per FIT file; a timer stop/start begins a new segment. Laps become lap
/// waypoints at the position where each lap ended.
pub fn parse(bytes: &[u8]) -> Result<ParsedFile> {
    let records = decode_lenient(bytes)?;

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

/// Decodes message by message and keeps everything before the first error, so files cut
/// off by a power loss (bad CRC, truncated last message) still yield their points.
fn decode_lenient(mut buffer: &[u8]) -> Result<Vec<FitDataRecord>> {
    let mut processor = FitStreamProcessor::new();
    processor.add_option(DecodeOption::SkipHeaderCrcValidation);
    processor.add_option(DecodeOption::SkipDataCrcValidation);
    let mut records = Vec::new();
    while !buffer.is_empty() {
        let (rest, obj) = match processor.deserialize_next(buffer) {
            Ok(r) => r,
            Err(e) if records.is_empty() => return Err(anyhow!("FIT decode failed: {e}")),
            Err(_) => break,
        };
        match obj {
            FitObject::Crc(_) => processor.reset(),
            FitObject::DataMessage(msg) => match processor.decode_message(msg) {
                Ok(rec) => records.push(rec),
                Err(e) if records.is_empty() => return Err(anyhow!("FIT decode failed: {e}")),
                Err(_) => break,
            },
            _ => {}
        }
        buffer = rest;
    }
    Ok(records)
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
