use anyhow::{Result, bail};

use crate::model::{ParsedFile, ParsedTrack, Points};

const RECORD: usize = 16;

/// Holux M-241 log: 16-byte records of UTC seconds (u32), latitude and longitude (f32),
/// altitude as an f32 without its lowest byte, and an XOR checksum over the other 15 bytes.
/// Records with a wrong checksum are skipped.
pub fn parse(bytes: &[u8]) -> Result<ParsedFile> {
    let mut points = Points::default();
    let mut valid = 0usize;
    for r in bytes.as_chunks::<RECORD>().0 {
        if r[..15].iter().fold(0u8, |x, b| x ^ b) != r[15] {
            continue;
        }
        valid += 1;
        let time = u32::from_le_bytes(r[0..4].try_into().unwrap());
        let lat = f32::from_le_bytes(r[4..8].try_into().unwrap()) as f64;
        let lon = f32::from_le_bytes(r[8..12].try_into().unwrap()) as f64;
        let ele = f32::from_le_bytes([0, r[12], r[13], r[14]]);
        if lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 {
            let ele = ele.is_finite().then_some(ele);
            points.push(lat, lon, ele, (time > 0).then_some(time as i64 * 1000));
        }
    }
    if valid == 0 && !bytes.is_empty() {
        bail!("not a Holux track log: no valid record");
    }
    Ok(ParsedFile {
        tracks: vec![ParsedTrack { name: None, description: None, activity: None, points }],
        waypoints: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(time: u32, lat: f32, lon: f32, ele: f32) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(&time.to_le_bytes());
        r.extend_from_slice(&lat.to_le_bytes());
        r.extend_from_slice(&lon.to_le_bytes());
        r.extend_from_slice(&ele.to_le_bytes()[1..]);
        r.push(r.iter().fold(0, |x, b| x ^ b));
        r
    }

    #[test]
    fn decodes_records_and_skips_bad_checksums() {
        let mut bytes = record(1207926092, 50.125, 8.5, 191.25);
        let mut bad = record(1207926100, 1.0, 2.0, 3.0);
        bad[15] ^= 1;
        bytes.extend(bad);
        bytes.extend(record(1207926288, 50.25, 8.75, 180.8125));
        let t = &parse(&bytes).unwrap().tracks[0];
        assert_eq!(t.points.len(), 2);
        assert_eq!((t.points.lat[0], t.points.lon[0]), (50.125, 8.5));
        assert_eq!(t.points.ele[0], 191.25);
        assert_eq!(t.points.time[1], 1207926288000);
    }

    #[test]
    fn rejects_other_binary_files() {
        assert!(parse(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><gpx></gpx>").is_err());
    }
}
