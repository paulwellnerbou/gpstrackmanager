use anyhow::{Result, bail};
use chrono::{Days, NaiveDate};

use crate::model::{ParsedFile, ParsedTrack, Points};

struct Fix {
    /// Milliseconds since midnight UTC.
    tod: i64,
    date: Option<NaiveDate>,
    lat: f64,
    lon: f64,
    ele: Option<f32>,
}

/// Reads RMC (position, date) and GGA (position, altitude) sentences of any talker. Sentences
/// of the same second are one fix. GGA carries no date, so it continues from the last RMC
/// date and rolls over at midnight. Sentences with a wrong checksum are skipped.
pub fn parse(bytes: &[u8]) -> Result<ParsedFile> {
    let text = String::from_utf8_lossy(bytes);
    let sentences: Vec<Vec<&str>> = text.lines().filter_map(sentence).collect();
    if sentences.is_empty() && !text.trim().is_empty() {
        bail!("no NMEA sentences found");
    }

    // Fixes before the first RMC take its date.
    let mut date = sentences.iter().find_map(|s| if kind(s) == "RMC" { rmc_date(s) } else { None });
    let mut last_tod: Option<i64> = None;
    let mut points = Points::default();
    let mut pending: Option<Fix> = None;
    let mut flush = |fix: Fix, points: &mut Points| {
        if let Some(d) = fix.date {
            date = Some(d);
        } else if let (Some(d), Some(last)) = (date, last_tod)
            && fix.tod < last
        {
            date = d.checked_add_days(Days::new(1));
        }
        last_tod = Some(fix.tod);
        let time = date.map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis() + fix.tod);
        points.push(fix.lat, fix.lon, fix.ele, time);
    };

    for s in &sentences {
        let fix = match kind(s) {
            "RMC" if s.get(2) == Some(&"A") => position(s, 3).zip(time_of_day(s[1])).map(|((lat, lon), tod)| Fix {
                tod,
                date: rmc_date(s),
                lat,
                lon,
                ele: None,
            }),
            "GGA" if s.get(6).is_some_and(|q| !q.is_empty() && *q != "0") => {
                position(s, 2).zip(time_of_day(s[1])).map(|((lat, lon), tod)| Fix {
                    tod,
                    date: None,
                    lat,
                    lon,
                    ele: s.get(9).and_then(|e| e.trim().parse::<f32>().ok()).filter(|e| e.is_finite()),
                })
            }
            _ => None,
        };
        let Some(fix) = fix else { continue };
        match pending.as_mut() {
            Some(p) if p.tod == fix.tod => {
                p.date = p.date.or(fix.date);
                p.ele = p.ele.or(fix.ele);
            }
            _ => {
                if let Some(p) = pending.replace(fix) {
                    flush(p, &mut points);
                }
            }
        }
    }
    if let Some(p) = pending {
        flush(p, &mut points);
    }
    Ok(ParsedFile {
        tracks: vec![ParsedTrack { name: None, description: None, activity: None, points }],
        waypoints: Vec::new(),
    })
}

/// Fields of a `$..` sentence with a valid or absent checksum, without the checksum.
fn sentence(line: &str) -> Option<Vec<&str>> {
    let line = &line[line.find('$')? + 1..];
    let line = line.trim_end();
    let body = match line.rsplit_once('*') {
        Some((body, sum)) => {
            let want = u8::from_str_radix(sum.get(..2)?, 16).ok()?;
            if body.bytes().fold(0, |x, b| x ^ b) != want {
                return None;
            }
            body
        }
        None => line,
    };
    Some(body.split(',').collect())
}

/// Sentence type without the talker (`GPRMC`, `GNRMC` → `RMC`).
fn kind<'a>(s: &[&'a str]) -> &'a str {
    s[0].get(2..).unwrap_or("")
}

fn time_of_day(s: &str) -> Option<i64> {
    let s = s.trim();
    let (hms, frac) = s.split_once('.').unwrap_or((s, ""));
    if hms.len() != 6 || !hms.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |i: usize| hms[i..i + 2].parse::<i64>().unwrap();
    let ms = if frac.is_empty() { 0 } else { (format!("0.{frac}").parse::<f64>().ok()? * 1000.0).round() as i64 };
    let (h, m, sec) = (n(0), n(2), n(4));
    (h < 24 && m < 60 && sec < 61).then_some(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

fn rmc_date(s: &[&str]) -> Option<NaiveDate> {
    let d = s.get(9)?.trim();
    if d.len() != 6 {
        return None;
    }
    let n = |i: usize| d.get(i..i + 2)?.parse::<u32>().ok();
    let yy = n(4)? as i32;
    NaiveDate::from_ymd_opt(if yy < 80 { 2000 + yy } else { 1900 + yy }, n(2)?, n(0)?)
}

/// `ddmm.mmmm,N,dddmm.mmmm,E` starting at field `i`. Some loggers drop or space-pad the
/// leading zeros of the degrees, which the numeric split handles.
fn position(s: &[&str], i: usize) -> Option<(f64, f64)> {
    let coord = |v: &str, hemi: &str, neg: &str| -> Option<f64> {
        let v = v.trim().parse::<f64>().ok()?;
        let deg = (v / 100.0).trunc();
        let d = deg + (v - deg * 100.0) / 60.0;
        Some(if hemi.trim().eq_ignore_ascii_case(neg) { -d } else { d })
    };
    let lat = coord(s.get(i)?, s.get(i + 1)?, "S")?;
    let lon = coord(s.get(i + 2)?, s.get(i + 3)?, "W")?;
    (lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some((lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_sum(body: &str) -> String {
        format!("${body}*{:02X}", body.bytes().fold(0u8, |x, b| x ^ b))
    }

    #[test]
    fn reads_holux_rmc_with_short_degrees() {
        let f = parse(b"$GPRMC,150132,A,5007.659073,N,831.270695,E,0,0,110408,,*1c\r\n").unwrap();
        let p = &f.tracks[0].points;
        assert_eq!(p.len(), 1);
        assert!((p.lat[0] - 50.127651).abs() < 1e-6);
        assert!((p.lon[0] - 8.521178).abs() < 1e-6);
        assert_eq!(p.time[0], 1207926092000);
        assert!(p.ele[0].is_nan());
    }

    #[test]
    fn merges_rmc_and_gga_of_one_second_and_rolls_over_midnight() {
        let lines = [
            with_sum("GPGGA,235959,4807.038,N,01131.000,W,1,08,0.9,545.4,M,46.9,M,,"),
            with_sum("GPRMC,235959,A,4807.038,N,01131.000,W,022.4,084.4,311299,003.1,W"),
            with_sum("GPGSA,A,3,04,05,,09,12,,,24,,,,,2.5,1.3,2.1"),
            with_sum("GPGGA,000001.5,4807.040,S,01131.002,W,1,08,0.9,546.0,M,46.9,M,,"),
            with_sum("GPGGA,000002,4807.040,N,01131.002,W,0,00,,,M,,M,,"),
            with_sum("GPRMC,000003,V,4807.040,N,01131.002,W,,,010100,,"),
            "$GPRMC,000004,A,4807.040,N,01131.002,W,,,010100,,*00".to_string(),
        ];
        let p = &parse(lines.join("\n").as_bytes()).unwrap().tracks[0].points;
        assert_eq!(p.len(), 2);
        assert_eq!(p.ele[0], 545.4);
        assert!(p.lon[0] < 0.0 && p.lat[1] < 0.0);
        assert_eq!(p.time[0], 946684799000);
        assert_eq!(p.time[1], 946684801500);
    }

    #[test]
    fn rejects_text_without_sentences() {
        assert!(parse(b"hello\nworld\n").is_err());
        assert_eq!(parse(b"").unwrap().tracks[0].points.len(), 0);
    }
}
