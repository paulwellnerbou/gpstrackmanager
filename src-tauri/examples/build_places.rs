//! Builds `data/places.tsv.gz` from a GeoNames dump (https://download.geonames.org/export/dump/):
//! `cargo run --example build_places -- cities1000.txt admin1CodesASCII.txt countryInfo.txt data/places.tsv.gz`
//!
//! Output lines: `C cc name`, `R cc.admin1 name`, `P name lat lon population cc.admin1`.

use std::collections::BTreeMap;
use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;

/// Districts, historical, abandoned and destroyed places would win over the town they lie in.
const SKIP_FEATURES: &[&str] = &["PPLX", "PPLH", "PPLQ", "PPLW", "PPLCH"];

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, cities, admin1, countries, out] = args.as_slice() else {
        anyhow::bail!("usage: build_places <cities.txt> <admin1CodesASCII.txt> <countryInfo.txt> <out.tsv.gz>");
    };
    let mut gz = GzEncoder::new(std::fs::File::create(out)?, Compression::best());

    for line in std::fs::read_to_string(countries)?.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() > 4 {
            writeln!(gz, "C\t{}\t{}", f[0], f[4])?;
        }
    }
    for line in std::fs::read_to_string(admin1)?.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() > 1 {
            writeln!(gz, "R\t{}\t{}", f[0], f[1])?;
        }
    }

    // Sorted by region so neighbouring lines share their tail, which compresses well.
    let mut places: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let (mut kept, mut skipped) = (0, 0);
    for line in std::fs::read_to_string(cities)?.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 15 || f[6] != "P" || SKIP_FEATURES.contains(&f[7]) {
            skipped += 1;
            continue;
        }
        let (name, lat, lon, cc, adm1, pop) = (f[1], f[4], f[5], f[8], f[10], f[14]);
        let (lat, lon): (f64, f64) = (lat.parse()?, lon.parse()?);
        let region = if adm1.is_empty() || adm1 == "00" { cc.to_string() } else { format!("{cc}.{adm1}") };
        places
            .entry(region.clone())
            .or_default()
            .push(format!("P\t{name}\t{lat:.4}\t{lon:.4}\t{}\t{region}", pop.parse::<u64>().unwrap_or(0)));
        kept += 1;
    }
    for lines in places.values() {
        for l in lines {
            writeln!(gz, "{l}")?;
        }
    }
    gz.finish()?;
    eprintln!("{kept} places, {skipped} skipped → {out}");
    Ok(())
}
