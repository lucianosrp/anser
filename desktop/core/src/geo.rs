//! Offline geography for the travel map: country outlines and a city
//! gazetteer, bundled into the binary so nothing is fetched and no map
//! service learns where the owner travels.
//!
//! * `assets/geo/countries.bin`: Natural Earth 1:110m countries (public
//!   domain), outer rings in lon/lat at 0.01°.
//! * `assets/geo/cities.tsv`: GeoNames places with 100k+ people (CC BY 4.0),
//!   biggest first: country, names, lat, lon.
//!
//! Both are built by `cargo run -p finance-core --example geo_assets`
//! (examples/geo_assets.rs); see the LICENSE files beside
//! them. Parsed once, on first use.

use std::collections::HashMap;
use std::sync::OnceLock;

const COUNTRIES: &[u8] = include_bytes!("../assets/geo/countries.bin");
const CITIES: &str = include_str!("../assets/geo/cities.tsv");

/// A point on the globe, degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LonLat {
    pub lon: f32,
    pub lat: f32,
}

pub struct Country {
    /// ISO 3166-1 alpha-2
    pub iso: String,
    /// where a label (or a country-level dot) goes
    pub label: LonLat,
    /// outer rings, lon/lat
    pub rings: Vec<Vec<LonLat>>,
    /// (min lon, min lat, max lon, max lat)
    pub bbox: (f32, f32, f32, f32),
}

#[derive(Debug)]
pub struct City {
    pub cc: String,
    /// display name (GeoNames' ASCII name)
    pub name: String,
    pub at: LonLat,
}

pub fn countries() -> &'static [Country] {
    static C: OnceLock<Vec<Country>> = OnceLock::new();
    C.get_or_init(|| parse_countries(COUNTRIES))
}

fn parse_countries(b: &[u8]) -> Vec<Country> {
    let mut i = 0;
    let rd_u16 = |i: &mut usize| {
        let v = u16::from_le_bytes([b[*i], b[*i + 1]]);
        *i += 2;
        v
    };
    let rd_deg = |i: &mut usize| {
        let v = i16::from_le_bytes([b[*i], b[*i + 1]]);
        *i += 2;
        v as f32 / 100.
    };
    let n = rd_u16(&mut i);
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let iso = String::from_utf8_lossy(&b[i..i + 2]).into_owned();
        i += 2;
        let label = LonLat { lon: rd_deg(&mut i), lat: rd_deg(&mut i) };
        let nr = rd_u16(&mut i);
        let mut rings = Vec::with_capacity(nr as usize);
        let mut bbox = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for _ in 0..nr {
            let np = rd_u16(&mut i);
            let ring: Vec<LonLat> = (0..np).map(|_| LonLat { lon: rd_deg(&mut i), lat: rd_deg(&mut i) }).collect();
            for p in &ring {
                bbox = (bbox.0.min(p.lon), bbox.1.min(p.lat), bbox.2.max(p.lon), bbox.3.max(p.lat));
            }
            rings.push(ring);
        }
        out.push(Country { iso, label, rings, bbox });
    }
    out
}

struct Gazetteer {
    cities: Vec<City>,
    /// lowercased, ASCII-folded name -> city indices, biggest first
    by_name: HashMap<String, Vec<usize>>,
}

fn gazetteer() -> &'static Gazetteer {
    static G: OnceLock<Gazetteer> = OnceLock::new();
    G.get_or_init(|| {
        let mut cities = Vec::new();
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for line in CITIES.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            let [cc, names, lat, lon] = f.as_slice() else { continue };
            let (Ok(lat), Ok(lon)) = (lat.parse(), lon.parse()) else { continue };
            let idx = cities.len();
            let mut names = names.split('|');
            let display = names.next().unwrap_or_default();
            for n in std::iter::once(display).chain(names) {
                let key = fold(n);
                let v = by_name.entry(key).or_default();
                if !v.contains(&idx) {
                    v.push(idx);
                }
            }
            cities.push(City { cc: cc.to_string(), name: display.to_string(), at: LonLat { lon, lat } });
        }
        Gazetteer { cities, by_name }
    })
}

/// Lowercase, accents dropped, punctuation as spaces: "Île-de-France" ->
/// "ile de france".
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'a',
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => 'u',
            'ç' | 'Ç' => 'c',
            'ñ' | 'Ñ' => 'n',
            'ß' => 's',
            c if c.is_alphanumeric() => c,
            '\'' | '’' => continue,
            _ => ' ',
        };
        out.extend(c.to_lowercase());
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Single words that are city names somewhere but mostly mean something else
/// on a receipt.
const NOT_CITIES: [&str; 22] = [
    "bar", "nice", "tours", "split", "mobile", "sale", "grand", "central", "victoria", "union", "university",
    "market", "marche", "bay", "deal", "bath", "reading", "orange", "santa", "como", "ancona", "plus",
];

/// The city a receipt names, among `countries` (ISO codes; empty = any).
/// Longer names win ("Reggio Calabria" over "Reggio"), then the ones nearest
/// the end (a card row ends "… BARI IT EUR 12.50"), then the biggest city.
pub fn find_city(text: &str, countries: &[&str]) -> Option<&'static City> {
    let g = gazetteer();
    let words: Vec<String> = fold(text).split(' ').filter(|w| !w.is_empty()).map(String::from).collect();
    for n in (1..=3).rev() {
        for start in (0..words.len().saturating_sub(n - 1)).rev() {
            let key = words[start..start + n].join(" ");
            if n == 1 && (key.len() < 4 || NOT_CITIES.contains(&key.as_str()) || key.chars().any(|c| c.is_ascii_digit())) {
                continue;
            }
            if let Some(hit) = g.by_name.get(&key).and_then(|ix| {
                ix.iter().map(|&i| &g.cities[i]).find(|c| countries.is_empty() || countries.contains(&c.cc.as_str()))
            }) {
                return Some(hit);
            }
        }
    }
    None
}

/// A city by exact name in a country ("Tokyo", "JP").
pub fn city(name: &str, cc: &str) -> Option<&'static City> {
    let g = gazetteer();
    g.by_name.get(&fold(name))?.iter().map(|&i| &g.cities[i]).find(|c| c.cc == cc)
}

/// Where a country-level dot goes: the polygon's label point, or for places
/// too small for 1:110m (Hong Kong, Macau, Singapore) the biggest city.
/// "EU" (somewhere in the euro area) is central Europe.
pub fn country_point(cc: &str) -> Option<LonLat> {
    if cc == "EU" {
        return Some(LonLat { lon: 10.0, lat: 48.5 });
    }
    if let Some(c) = gazetteer().cities.iter().find(|c| c.cc == cc).filter(|_| matches!(cc, "HK" | "MO" | "SG")) {
        return Some(c.at);
    }
    countries().iter().find(|c| c.iso == cc).map(|c| c.label)
        .or_else(|| gazetteer().cities.iter().find(|c| c.cc == cc).map(|c| c.at))
}

/// Great-circle distance, km.
pub fn km(a: LonLat, b: LonLat) -> f32 {
    let r = |d: f32| d.to_radians();
    let (dlat, dlon) = (r(b.lat - a.lat), r(b.lon - a.lon));
    let h = (dlat / 2.).sin().powi(2) + r(a.lat).cos() * r(b.lat).cos() * (dlon / 2.).sin().powi(2);
    2. * 6371. * h.sqrt().asin()
}

/// European countries: where a EUR account's card may have been used.
pub const EUROPE: [&str; 33] = [
    "AT", "BE", "HR", "CY", "EE", "FI", "FR", "DE", "GR", "IE", "IT", "LV", "LT", "LU", "MT", "NL", "PT", "SK", "SI", "ES",
    "CH", "GB", "NO", "SE", "DK", "PL", "CZ", "HU", "RO", "BG", "AL", "ME", "IS",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_parse() {
        let c = countries();
        assert!(c.len() > 170, "{}", c.len());
        let it = c.iter().find(|c| c.iso == "IT").expect("Italy");
        assert!(it.rings.len() >= 3, "mainland, Sicily, Sardinia");
        assert!(it.bbox.0 > 6. && it.bbox.2 < 19.);
        assert!(c.iter().any(|c| c.iso == "FR") && c.iter().any(|c| c.iso == "NO") && c.iter().any(|c| c.iso == "TW"));
        assert!(gazetteer().cities.len() > 5000);
    }

    #[test]
    fn finds_cities_in_receipts() {
        let name = |t: &str, cc: &[&str]| find_city(t, cc).map(|c| c.name.as_str());
        assert_eq!(name("MATSUZAKAYA NAGOYA JP JPY 780", &["JP"]), Some("Nagoya"));
        assert_eq!(name("AUTOLINEE BIANCHI S.R.L. BARI IT EUR 12.50", &["IT"]), Some("Bari"));
        assert_eq!(name("PETIT FOUR LYON 3", &EUROPE), Some("Lyon"));
        assert_eq!(name("Aeroporto de Lisboa", &EUROPE), Some("Lisbon"));
        assert_eq!(name("HAWKER CORNER SINGAPORE SG SGD 12.00", &["SG"]), Some("Singapore"));
        assert_eq!(name("Parkhotel Wien", &EUROPE), Some("Vienna"));
        // not in the allowed countries, or not a place at all
        assert_eq!(name("Nagoya Ramen House", &EUROPE), None);
        assert_eq!(name("BAR DU PORT - HONFLEUR", &EUROPE), None);
        assert_eq!(name("Miami Beach S.n.c. Di Lu", &EUROPE), None);
    }

    #[test]
    fn points_and_distances() {
        let hk = country_point("HK").unwrap();
        assert!((hk.lat - 22.3).abs() < 0.5 && (hk.lon - 114.2).abs() < 0.5);
        let jp = country_point("JP").unwrap();
        assert!(jp.lon > 130. && jp.lon < 145.);
        let tokyo = city("Tokyo", "JP").unwrap().at;
        let d = km(hk, tokyo);
        assert!((2800. ..3000.).contains(&d), "{d}");
    }
}
