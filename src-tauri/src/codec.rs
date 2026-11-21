//! Little-endian blob encodings for point columns stored in SQLite.

/// Coordinates are stored as 1e-7 degree integers (~1 cm), like FIT does.
const COORD_SCALE: f64 = 1e7;
/// Marks a segment break inside an encoded coordinate list.
pub const BREAK: i32 = i32::MIN;

pub fn encode_coords(lat: &[f64], lon: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(lat.len() * 8);
    for i in 0..lat.len() {
        out.extend_from_slice(&((lat[i] * COORD_SCALE).round() as i32).to_le_bytes());
        out.extend_from_slice(&((lon[i] * COORD_SCALE).round() as i32).to_le_bytes());
    }
    out
}

/// Decodes to (lat, lon) pairs; segment breaks become NaN pairs.
pub fn decode_coords(bytes: &[u8]) -> Vec<(f64, f64)> {
    bytes
        .chunks_exact(8)
        .map(|c| {
            let lat = i32::from_le_bytes(c[0..4].try_into().unwrap());
            let lon = i32::from_le_bytes(c[4..8].try_into().unwrap());
            if lat == BREAK {
                (f64::NAN, f64::NAN)
            } else {
                (lat as f64 / COORD_SCALE, lon as f64 / COORD_SCALE)
            }
        })
        .collect()
}

pub fn push_coord(out: &mut Vec<u8>, lat: f64, lon: f64) {
    out.extend_from_slice(&((lat * COORD_SCALE).round() as i32).to_le_bytes());
    out.extend_from_slice(&((lon * COORD_SCALE).round() as i32).to_le_bytes());
}

pub fn push_break(out: &mut Vec<u8>) {
    out.extend_from_slice(&BREAK.to_le_bytes());
    out.extend_from_slice(&BREAK.to_le_bytes());
}

pub fn encode_f32(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn decode_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}

pub fn encode_i64(v: &[i64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn decode_i64(b: &[u8]) -> Vec<i64> {
    b.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect()
}

pub fn encode_u32(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn decode_u32(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}
