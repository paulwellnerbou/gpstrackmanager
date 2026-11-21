use std::f64::consts::PI;

const EARTH_RADIUS_M: f64 = 6_371_008.8;

pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().asin()
}

/// Initial great-circle bearing from point 1 to point 2, degrees clockwise from north.
pub fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// Web-Mercator world coordinates normalised to [0, 1], the space MapLibre renders in,
/// so a tolerance of one screen pixel at zoom z is exactly `1 / (512 * 2^z)`.
pub fn mercator(lat: f64, lon: f64) -> (f64, f64) {
    let lat = lat.clamp(-85.051_128, 85.051_128).to_radians();
    let x = (lon + 180.0) / 360.0;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / PI) / 2.0;
    (x, y)
}

pub fn pixel_size_at_zoom(zoom: f64) -> f64 {
    1.0 / (512.0 * 2f64.powf(zoom))
}
