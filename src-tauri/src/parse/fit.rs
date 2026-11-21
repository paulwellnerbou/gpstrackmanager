use anyhow::{Result, anyhow};
use fitparser::de::{DecodeOption, from_bytes_with_options};
use fitparser::profile::MesgNum;
use fitparser::{FitDataRecord, Value};

use crate::model::{ParsedTrack, Points};

const SEMICIRCLE_TO_DEG: f64 = 180.0 / 2_147_483_648.0;

/// One track per FIT file; a timer stop/start begins a new segment.
pub fn parse(bytes: &[u8]) -> Result<Vec<ParsedTrack>> {
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
            MesgNum::Session if sport.is_none() => {
                if let Some(Value::String(s)) = field(rec, "sport") {
                    sport = Some(s.clone());
                }
            }
            _ => {}
        }
    }
    let activity = sport.as_deref().and_then(crate::activity::normalize);
    Ok(vec![ParsedTrack { name: None, description: None, activity, points }])
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
