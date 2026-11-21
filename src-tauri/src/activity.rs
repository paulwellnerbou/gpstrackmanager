/// Maps activity names used by FIT (`sport`) and GPX (`<type>`, e.g. Strava, Garmin)
/// to the app's activity keys. Unknown or generic values yield `None`; numeric type codes
/// are ignored because their meaning differs between vendors.
pub fn normalize(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    let key = match s.as_str() {
        "walking" | "walk" | "indoor_walking" => "walking",
        "hiking" | "hike" | "mountaineering" => "hiking",
        "running" | "run" | "trail_running" | "street_running" => "running",
        "cycling" | "biking" | "bike" | "ride" | "e_biking" | "road_biking" => "bike",
        "mountain_biking" | "mtb" | "mountainbike" => "mtb",
        "driving" | "car" | "motor_vehicle" => "car",
        "motorcycling" | "motorbike" => "motorbike",
        "train" | "rail" => "train",
        "bus" => "bus",
        "boating" | "boat" | "sailing" | "rowing" | "kayaking" | "paddling" | "stand_up_paddleboarding" => "boat",
        "flying" | "flight" | "airplane" | "aeroplane" | "plane" => "aeroplane",
        "horseback_riding" | "riding" | "horse_riding" => "riding",
        "alpine_skiing" | "cross_country_skiing" | "skiing" | "snowboarding" | "backcountry_skiing" => "skiing",
        "swimming" | "open_water_swimming" => "swimming",
        _ => return None,
    };
    Some(key.to_string())
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn maps_common_names() {
        assert_eq!(normalize("Cycling").as_deref(), Some("bike"));
        assert_eq!(normalize("horseback_riding").as_deref(), Some("riding"));
        assert_eq!(normalize("generic"), None);
    }
}
