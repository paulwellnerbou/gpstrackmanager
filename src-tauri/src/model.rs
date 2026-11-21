/// Sentinel for points without a timestamp.
pub const NO_TIME: i64 = i64::MIN;

/// Columnar point storage; columnar keeps huge tracks compact and cache friendly.
#[derive(Default, Debug)]
pub struct Points {
    pub lat: Vec<f64>,
    pub lon: Vec<f64>,
    /// NaN when unknown.
    pub ele: Vec<f32>,
    /// Unix milliseconds, `NO_TIME` when unknown.
    pub time: Vec<i64>,
    /// Start index of every segment (GPX `trkseg`).
    pub seg_starts: Vec<u32>,
}

impl Points {
    pub fn len(&self) -> usize {
        self.lat.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lat.is_empty()
    }

    pub fn start_segment(&mut self) {
        let n = self.len() as u32;
        if self.seg_starts.last() != Some(&n) {
            self.seg_starts.push(n);
        }
    }

    pub fn push(&mut self, lat: f64, lon: f64, ele: Option<f32>, time: Option<i64>) {
        // Loggers without a fix write 0/0 ("null island") instead of omitting the point.
        if lat == 0.0 && lon == 0.0 {
            return;
        }
        if self.seg_starts.is_empty() {
            self.seg_starts.push(0);
        }
        self.lat.push(lat);
        self.lon.push(lon);
        self.ele.push(ele.filter(|e| (-500.0..=9000.0).contains(e)).unwrap_or(f32::NAN));
        self.time.push(time.unwrap_or(NO_TIME));
    }

    /// Non-empty `[start, end)` ranges of all segments.
    pub fn segments(&self) -> Vec<(usize, usize)> {
        let n = self.len();
        let mut out = Vec::with_capacity(self.seg_starts.len());
        for (i, &s) in self.seg_starts.iter().enumerate() {
            let e = self.seg_starts.get(i + 1).map(|&e| e as usize).unwrap_or(n);
            if (s as usize) < e {
                out.push((s as usize, e));
            }
        }
        out
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Waypoint {
    pub name: Option<String>,
    pub description: Option<String>,
    pub symbol: Option<String>,
    /// "waypoint" or "lap" (lap markers from FIT or GPS loggers).
    pub kind: &'static str,
    pub lat: f64,
    pub lon: f64,
    pub ele: Option<f32>,
    pub time: Option<i64>,
}

impl Waypoint {
    /// Converters write FIT laps as waypoints named `LAP001`, …
    pub fn kind_for_name(name: Option<&str>) -> &'static str {
        match name {
            Some(n) if n.len() > 3 && n[..3].eq_ignore_ascii_case("lap") && n[3..].trim().parse::<u32>().is_ok() => {
                "lap"
            }
            _ => "waypoint",
        }
    }
}

#[derive(Debug, Default)]
pub struct ParsedFile {
    pub tracks: Vec<ParsedTrack>,
    pub waypoints: Vec<Waypoint>,
}

#[derive(Debug)]
pub struct ParsedTrack {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Normalised activity key (see `activity::normalize`), if the file states one.
    pub activity: Option<String>,
    pub points: Points,
}
