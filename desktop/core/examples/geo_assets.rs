//! Build the travel map's offline geo assets (desktop/core/assets/geo/).
//!
//!     cargo run -p finance-core --example geo_assets -- NE_110M_COUNTRIES.geojson CITIES15000.txt
//!
//! Inputs (download once; nothing is fetched at runtime):
//!   * Natural Earth 1:110m Admin 0 countries (public domain)
//!     https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_110m_admin_0_countries.geojson
//!   * GeoNames cities15000 (CC BY 4.0)
//!     https://download.geonames.org/export/dump/cities15000.zip
//!
//! Outputs:
//!   * countries.bin -- little endian:
//!       u16 n_countries, then per country:
//!         [u8;2] ISO 3166-1 alpha-2, i16 label lon*100, i16 label lat*100,
//!         u16 n_rings, then per ring: u16 n_points, n_points x (i16 lon*100, i16 lat*100)
//!   * cities.tsv -- "CC\tname[|other name...]\tlat\tlon" for every populated
//!     place with 100k+ people (city sections like "Paris 15 Vaugirard" dropped),
//!     biggest first, plus the local names card statements use (ROMA, MILANO...).
//!
//! An optional third argument writes to that directory instead of the assets dir.

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::Value;

const MIN_POP: i64 = 100_000;

/// Local / alternate names merchants print (GeoNames' main name is English).
const EXTRA_NAMES: &[(&str, &str, &[&str])] = &[
    ("IT", "Rome", &["Roma"]),
    ("IT", "Milan", &["Milano"]),
    ("IT", "Naples", &["Napoli"]),
    ("IT", "Turin", &["Torino"]),
    ("IT", "Florence", &["Firenze"]),
    ("IT", "Venice", &["Venezia"]),
    ("IT", "Genoa", &["Genova"]),
    ("IT", "Padua", &["Padova"]),
    ("IT", "Syracuse", &["Siracusa"]),
    ("DE", "Munich", &["Muenchen", "Munchen", "München"]),
    ("DE", "Cologne", &["Koeln", "Köln"]),
    ("DE", "Nuremberg", &["Nuernberg", "Nürnberg"]),
    ("AT", "Vienna", &["Wien"]),
    ("PT", "Lisbon", &["Lisboa"]),
    ("PT", "Porto", &["Oporto"]),
    ("CZ", "Prague", &["Praha"]),
    ("BE", "Brussels", &["Bruxelles", "Brussel"]),
    ("BE", "Antwerpen", &["Antwerp"]),
    ("NL", "The Hague", &["Den Haag"]),
    ("PL", "Warsaw", &["Warszawa"]),
    ("PL", "Krakow", &["Kraków"]),
    ("DK", "Copenhagen", &["Kobenhavn", "København"]),
    ("GR", "Athens", &["Athina"]),
    ("CH", "Zurich", &["Zürich"]),
    ("CH", "Geneva", &["Geneve", "Genève", "Genf"]),
    ("ES", "Seville", &["Sevilla"]),
    ("ES", "Saragossa", &["Zaragoza"]),
    ("CN", "Guangzhou", &["Canton"]),
    ("MO", "Macau", &["Macao"]),
    ("HK", "Hong Kong", &["Wanchai", "Sheung Wan", "Causeway Bay"]),
];

/// Degrees as hundredths, rounded half to even.
fn q(v: f64) -> i16 {
    (v * 100.0).round_ties_even() as i16
}

fn countries(path: &str) -> Vec<u8> {
    let d: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("read countries")).expect("geojson");
    let mut feats = Vec::new();
    for f in d["features"].as_array().expect("features") {
        let p = &f["properties"];
        let a2 = p["ISO_A2"].as_str().unwrap_or("");
        let iso = if a2 == "-99" || a2 == "CN-TW" { p["ISO_A2_EH"].as_str().unwrap_or("") } else { a2 };
        if iso.chars().count() != 2 || !iso.chars().all(char::is_alphabetic) {
            continue;
        }
        let g = &f["geometry"];
        let polys: Vec<&Value> = if g["type"] == "MultiPolygon" {
            g["coordinates"].as_array().unwrap().iter().collect()
        } else {
            vec![&g["coordinates"]]
        };
        // outer rings only
        let rings: Vec<&Value> = polys.iter().filter_map(|poly| poly.as_array().unwrap().first()).collect();
        feats.push((iso.to_string(), p["LABEL_X"].as_f64().unwrap(), p["LABEL_Y"].as_f64().unwrap(), rings));
    }
    let mut out = Vec::new();
    out.extend((feats.len() as u16).to_le_bytes());
    for (iso, lx, ly, rings) in &feats {
        out.extend(iso.as_bytes());
        out.extend(q(*lx).to_le_bytes());
        out.extend(q(*ly).to_le_bytes());
        out.extend((rings.len() as u16).to_le_bytes());
        for ring in rings {
            let mut pts: Vec<(i16, i16)> = Vec::new();
            for xy in ring.as_array().unwrap() {
                let pt = (q(xy[0].as_f64().unwrap()), q(xy[1].as_f64().unwrap()));
                if pts.last() != Some(&pt) {
                    pts.push(pt);
                }
            }
            out.extend((pts.len() as u16).to_le_bytes());
            for (x, y) in pts {
                out.extend(x.to_le_bytes());
                out.extend(y.to_le_bytes());
            }
        }
    }
    out
}

/// Accents dropped, any other non-ASCII char removed ("München" -> "Munchen",
/// "København" -> "Kbenhavn"): what a statement printed in plain ASCII shows.
fn ascii_fold(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            if c.is_ascii() {
                return Some(c);
            }
            let base = match c {
                'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
                'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'A',
                'ç' => 'c',
                'Ç' => 'C',
                'è' | 'é' | 'ê' | 'ë' => 'e',
                'È' | 'É' | 'Ê' | 'Ë' => 'E',
                'ì' | 'í' | 'î' | 'ï' => 'i',
                'Ì' | 'Í' | 'Î' | 'Ï' => 'I',
                'ñ' | 'ń' => 'n',
                'Ñ' | 'Ń' => 'N',
                'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
                'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => 'O',
                'ù' | 'ú' | 'û' | 'ü' => 'u',
                'Ù' | 'Ú' | 'Û' | 'Ü' => 'U',
                'ý' | 'ÿ' => 'y',
                'Ý' => 'Y',
                _ => return None,
            };
            Some(base)
        })
        .collect()
}

fn cities(path: &str) -> String {
    let text = std::fs::read_to_string(path).expect("read cities");
    let mut rows = Vec::new();
    for line in text.lines() {
        let r: Vec<&str> = line.split('\t').collect();
        let pop: i64 = if r[14].is_empty() { 0 } else { r[14].parse().expect("population") };
        if pop < MIN_POP || r[7] == "PPLX" {
            continue;
        }
        rows.push((pop, r[8], r[1], r[2], r[4].parse::<f64>().unwrap(), r[5].parse::<f64>().unwrap()));
    }
    // biggest first; equal populations keep file order
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    let mut seen = HashSet::new();
    let mut lines = Vec::new();
    for (_, cc, name, ascii_name, lat, lon) in rows {
        if !seen.insert((cc, ascii_name.to_lowercase())) {
            continue;
        }
        let mut names = vec![ascii_name.to_string()];
        if name != ascii_name {
            names.push(name.to_string());
        }
        if let Some((_, _, extra)) = EXTRA_NAMES.iter().find(|(c, n, _)| *c == cc && *n == ascii_name) {
            for e in *extra {
                names.push(e.to_string());
                let folded = ascii_fold(e);
                if folded != *e {
                    names.push(folded);
                }
            }
        }
        let mut uniq: Vec<String> = Vec::new();
        for n in names {
            let n = n.replace(['|', '\t'], " ");
            if !uniq.contains(&n) {
                uniq.push(n);
            }
        }
        lines.push(format!("{cc}\t{}\t{lat:.2}\t{lon:.2}", uniq.join("|")));
    }
    lines.join("\n") + "\n"
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [ne, gn, rest @ ..] = args.as_slice() else {
        eprintln!("usage: geo_assets NE_110M_COUNTRIES.geojson CITIES15000.txt [OUT_DIR]");
        std::process::exit(2);
    };
    let out = rest
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/geo"));
    std::fs::write(out.join("countries.bin"), countries(ne)).expect("write countries.bin");
    std::fs::write(out.join("cities.tsv"), cities(gn)).expect("write cities.tsv");
    eprintln!("wrote {}", out.display());
}
