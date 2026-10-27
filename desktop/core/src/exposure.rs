//! What the investments are exposed to: by trading currency, by listing
//! venue, by asset type, and, looking through the funds, by region and
//! sector.
//!
//! The look-through uses a small static table of **approximate** index
//! weights ([`INSTRUMENTS`], as of [`WEIGHTS_AS_OF`]). They are rounded
//! figures in the style of the index providers' factsheets, not live data:
//! index weights drift every day, so read them as "roughly". Single stocks
//! are themselves (100% their listing country, one sector: a widely held
//! company's from [`COMPANIES`], else the description's words). Each fund's
//! biggest names give the single-name look-through: a stock held directly
//! and inside the funds ([`HeldTwice`]). Crypto has no region or sector, and
//! anything else counts as "unknown", never guessed.
//!
//! Trading currency is the currency a holding is quoted and bought in. It is
//! not the currency risk underneath: a USD-quoted world ETF still holds yen
//! and euro companies. The region look-through is the closer view of that.

use std::collections::BTreeMap;

pub mod class;

/// When the static weights below were compiled (approximate figures).
pub const WEIGHTS_AS_OF: &str = "mid-2026";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Etf,
    Stock,
    /// a mutual fund (bought from a fund platform)
    Fund,
    Crypto,
    Cash,
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Etf => "ETFs",
            Kind::Stock => "Single stocks",
            Kind::Fund => "Mutual fund",
            Kind::Crypto => "Crypto",
            Kind::Cash => "Cash",
            Kind::Other => "Other",
        }
    }
}

/// Where a holding trades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Venue {
    /// London Stock Exchange
    Lse,
    /// a US exchange: NYSE Arca, Nasdaq
    Us,
    /// bought from the fund house or a fund platform
    FundHouse,
    /// exchanges and the cold wallet
    CryptoVenue,
    /// broker cash
    Account,
    Unknown,
}

impl Venue {
    pub fn label(self) -> &'static str {
        match self {
            Venue::Lse => "London (LSE)",
            Venue::Us => "US exchanges",
            Venue::FundHouse => "Fund platform",
            Venue::CryptoVenue => "Crypto exchanges / wallet",
            Venue::Account => "Broker cash",
            Venue::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Region {
    Us,
    Canada,
    Uk,
    EuropeExUk,
    Japan,
    PacificExJapan,
    Emerging,
}

impl Region {
    pub const ALL: [Region; 7] = [Region::Us, Region::Canada, Region::Uk, Region::EuropeExUk, Region::Japan, Region::PacificExJapan, Region::Emerging];
    pub fn label(self) -> &'static str {
        match self {
            Region::Us => "United States",
            Region::Canada => "Canada",
            Region::Uk => "United Kingdom",
            Region::EuropeExUk => "Europe ex-UK",
            Region::Japan => "Japan",
            Region::PacificExJapan => "Pacific ex-Japan",
            Region::Emerging => "Emerging markets",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Sector {
    Tech,
    Communication,
    ConsumerDisc,
    Financials,
    HealthCare,
    Industrials,
    /// staples, energy, materials, utilities, real estate
    Rest,
}

impl Sector {
    pub const ALL: [Sector; 7] = [Sector::Tech, Sector::Communication, Sector::ConsumerDisc, Sector::Financials, Sector::HealthCare, Sector::Industrials, Sector::Rest];
    pub fn label(self) -> &'static str {
        match self {
            Sector::Tech => "Information technology",
            Sector::Communication => "Communication services",
            Sector::ConsumerDisc => "Consumer discretionary",
            Sector::Financials => "Financials",
            Sector::HealthCare => "Health care",
            Sector::Industrials => "Industrials",
            Sector::Rest => "Everything else",
        }
    }
}

/// A fund (or index proxy) the look-through knows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Instrument {
    pub symbol: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    pub venue: Venue,
    /// what it tracks, in words
    pub index: &'static str,
    /// fund domicile (where withholding tax is decided)
    pub domicile: &'static str,
    /// % per region, approximate, summing to 100
    pub regions: &'static [(Region, f64)],
    /// % per GICS-style sector, approximate, summing to 100
    pub sectors: &'static [(Sector, f64)],
    /// its biggest single holdings: (stock symbol, % of the fund), for the
    /// single-name look-through
    pub top: &'static [(&'static str, f64)],
}

use Region::*;
use Sector::*;

/// FTSE All-World (large + mid caps, ~4k stocks), approximate.
const ALL_WORLD: &[(Region, f64)] = &[(Us, 62.), (Canada, 3.), (Uk, 3.), (EuropeExUk, 12.), (Japan, 5.), (PacificExJapan, 4.), (Emerging, 11.)];
/// FTSE Global All Cap (adds small caps, ~9k stocks): close to All-World.
const ALL_CAP: &[(Region, f64)] = &[(Us, 62.), (Canada, 3.), (Uk, 3.), (EuropeExUk, 12.), (Japan, 6.), (PacificExJapan, 4.), (Emerging, 10.)];
/// MSCI World: developed markets only.
const WORLD: &[(Region, f64)] = &[(Us, 72.), (Canada, 3.), (Uk, 3.), (EuropeExUk, 13.), (Japan, 5.5), (PacificExJapan, 3.5)];
/// Nasdaq-100: US-listed, a few foreign companies (ASML, AstraZeneca, Arm, PDD, MercadoLibre).
const NDX: &[(Region, f64)] = &[(Us, 96.), (Uk, 1.5), (EuropeExUk, 1.), (Emerging, 1.5)];

const GLOBAL_SECTORS: &[(Sector, f64)] = &[(Tech, 26.), (Financials, 17.), (Industrials, 11.), (ConsumerDisc, 10.), (HealthCare, 9.), (Communication, 8.), (Rest, 19.)];
const WORLD_SECTORS: &[(Sector, f64)] = &[(Tech, 27.), (Financials, 17.), (Industrials, 11.), (ConsumerDisc, 10.), (HealthCare, 9.5), (Communication, 8.5), (Rest, 17.)];
const NDX_SECTORS: &[(Sector, f64)] = &[(Tech, 52.), (Communication, 16.), (ConsumerDisc, 13.), (HealthCare, 5.), (Industrials, 4.), (Financials, 1.), (Rest, 9.)];

/// The biggest names of each index, % of it (approximate).
const ALL_WORLD_TOP: &[(&str, f64)] = &[("NVDA", 4.5), ("MSFT", 3.9), ("AAPL", 3.6), ("AMZN", 2.2), ("META", 1.6)];
const ALL_CAP_TOP: &[(&str, f64)] = &[("NVDA", 4.), ("MSFT", 3.5), ("AAPL", 3.2), ("AMZN", 2.), ("META", 1.4)];
const WORLD_TOP: &[(&str, f64)] = &[("NVDA", 5.), ("MSFT", 4.4), ("AAPL", 4.1), ("AMZN", 2.6), ("META", 1.9)];
const NDX_TOP: &[(&str, f64)] = &[("NVDA", 9.), ("MSFT", 8.5), ("AAPL", 7.5), ("AMZN", 5.5), ("AVGO", 5.), ("META", 3.5)];

/// The static table of broad index funds. Approximate weights as of
/// [`WEIGHTS_AS_OF`]; check the issuer's factsheet for today's. A fund
/// valued through an index proxy (the investment setup's) is looked
/// through as its proxy.
pub const INSTRUMENTS: &[Instrument] = &[
    Instrument { symbol: "VWRA", name: "Vanguard FTSE All-World UCITS ETF (USD, acc)", kind: Kind::Etf, venue: Venue::Lse,
        index: "FTSE All-World", domicile: "Ireland", regions: ALL_WORLD, sectors: GLOBAL_SECTORS, top: ALL_WORLD_TOP },
    Instrument { symbol: "VT", name: "Vanguard Total World Stock ETF", kind: Kind::Etf, venue: Venue::Us,
        index: "FTSE Global All Cap", domicile: "United States", regions: ALL_CAP, sectors: GLOBAL_SECTORS, top: ALL_CAP_TOP },
    Instrument { symbol: "EQQQ", name: "Invesco EQQQ Nasdaq-100 UCITS ETF (GBP, dist)", kind: Kind::Etf, venue: Venue::Lse,
        index: "Nasdaq-100", domicile: "Ireland", regions: NDX, sectors: NDX_SECTORS, top: NDX_TOP },
    Instrument { symbol: "QQQ", name: "Invesco QQQ Trust", kind: Kind::Etf, venue: Venue::Us,
        index: "Nasdaq-100", domicile: "United States", regions: NDX, sectors: NDX_SECTORS, top: NDX_TOP },
    Instrument { symbol: "URTH", name: "iShares MSCI World ETF", kind: Kind::Etf, venue: Venue::Us,
        index: "MSCI World", domicile: "United States", regions: WORLD, sectors: WORLD_SECTORS, top: WORLD_TOP },
];

/// Pairs that track the same (or nearly the same) index.
pub const OVERLAPS: &[(&str, &str, &str)] = &[
    ("VWRA", "VT", "FTSE All-World vs FTSE Global All Cap: the same global large and mid caps; VT adds small caps (about 10% of it). They move almost as one."),
    ("EQQQ", "QQQ", "Both track the Nasdaq-100: the same 100 companies at the same weights, one UCITS (Irish), one US-domiciled."),
];

pub fn instrument(symbol: &str) -> Option<&'static Instrument> {
    INSTRUMENTS.iter().find(|i| i.symbol.eq_ignore_ascii_case(symbol))
}

/// A widely held company: its name and sector (its region is where it is
/// listed).
pub struct Company {
    pub symbol: &'static str,
    pub name: &'static str,
    pub sector: Sector,
}

const fn co(symbol: &'static str, name: &'static str, sector: Sector) -> Company {
    Company { symbol, name, sector }
}

/// The largest US-listed companies (and every name in the funds' `top`).
pub const COMPANIES: &[Company] = &[
    co("NVDA", "NVIDIA", Tech),
    co("MSFT", "Microsoft", Tech),
    co("AAPL", "Apple", Tech),
    co("AVGO", "Broadcom", Tech),
    co("AMZN", "Amazon", ConsumerDisc),
    co("TSLA", "Tesla", ConsumerDisc),
    co("META", "Meta Platforms", Communication),
    co("GOOGL", "Alphabet", Communication),
    co("GOOG", "Alphabet", Communication),
    co("BRK.B", "Berkshire Hathaway", Financials),
    co("JPM", "JPMorgan Chase", Financials),
    co("V", "Visa", Financials),
    co("LLY", "Eli Lilly", HealthCare),
];

pub fn company(symbol: &str) -> Option<&'static Company> {
    COMPANIES.iter().find(|c| c.symbol.eq_ignore_ascii_case(symbol))
}

/// A single stock's sector from the broker's description, when its words
/// say ("... AEROSPACE ...", "... SEMICONDUCTOR ..."); None otherwise.
pub fn sector_in(description: &str) -> Option<Sector> {
    let w = format!(" {} ", description.to_uppercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" "));
    let has = |keys: &[&str]| keys.iter().any(|k| w.contains(&format!(" {k} ")));
    if has(&["SPACE", "AEROSPACE", "DEFENSE", "DEFENCE", "AIRLINES", "RAILROAD", "LOGISTICS", "MACHINERY"]) {
        Some(Industrials)
    } else if has(&["SEMICONDUCTOR", "SEMICONDUCTORS", "SOFTWARE", "MICROSYSTEMS"]) {
        Some(Tech)
    } else if has(&["BANK", "BANCORP", "FINANCIAL", "INSURANCE"]) {
        Some(Financials)
    } else if has(&["PHARMACEUTICAL", "PHARMACEUTICALS", "THERAPEUTICS", "BIOTECH", "HEALTH", "HEALTHCARE"]) {
        Some(HealthCare)
    } else {
        None
    }
}

/// The country a listing venue stands for (a single stock is 100% its own).
fn venue_region(v: Venue) -> Option<(Region, &'static str)> {
    match v {
        Venue::Us => Some((Region::Us, "United States")),
        Venue::Lse => Some((Region::Uk, "United Kingdom")),
        _ => None,
    }
}

/// Where a broker symbol trades: the table first, else by trading currency
/// (GBP is London; USD is a US listing unless the table says otherwise).
pub fn venue_of(symbol: &str, currency: &str) -> Venue {
    if let Some(i) = instrument(symbol) {
        return i.venue;
    }
    match currency {
        "GBP" | "GBX" => Venue::Lse,
        "USD" => Venue::Us,
        _ => Venue::Unknown,
    }
}

/// A holding's domicile, for the dividends' withholding: a fund's from the
/// table, a single stock's its listing country (`description`: the
/// broker's, which tells a company's shares). None: not known.
pub fn domicile_of(symbol: &str, description: &str, currency: &str) -> Option<&'static str> {
    match instrument(symbol) {
        Some(i) => Some(i.domicile),
        None if company(symbol).is_some() || class::classify(symbol, description).vehicle == Some(class::Vehicle::Stock) => {
            venue_region(venue_of(symbol, currency)).map(|r| r.1)
        }
        None => None,
    }
}

/// What a position looks through to: a table fund's weights, or a single
/// stock as itself (its listing country, its sector).
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub index: String,
    pub domicile: String,
    pub regions: Vec<(Region, f64)>,
    pub sectors: Vec<(Sector, f64)>,
    pub top: Vec<(&'static str, f64)>,
}

/// One thing held, valued in the base currency.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    /// a broker symbol, a fund's index proxy ("URTH"), a coin ("BTC"),
    /// "CASH", or anything else (counts as unknown)
    pub symbol: String,
    /// display name
    pub label: String,
    /// the broker's description ("NVIDIA CORP"), or ""
    pub description: String,
    /// trading currency
    pub currency: String,
    pub value_hkd: f64,
    pub kind: Kind,
    pub venue: Venue,
}

impl Position {
    /// A broker holding: kind and venue from the table, else from the
    /// description's words and the trading currency.
    pub fn broker(symbol: &str, description: &str, currency: &str, value_hkd: f64) -> Self {
        let kind = match instrument(symbol) {
            Some(i) => i.kind,
            None => match self::class::classify(symbol, description).vehicle {
                Some(class::Vehicle::Stock) => Kind::Stock,
                Some(class::Vehicle::Etf) => Kind::Etf,
                Some(class::Vehicle::Fund) => Kind::Fund,
                _ => Kind::Other,
            },
        };
        Position {
            symbol: symbol.into(),
            label: symbol.into(),
            description: description.into(),
            currency: currency.into(),
            value_hkd,
            kind,
            venue: venue_of(symbol, currency),
        }
    }

    /// Its look-through: the table's fund, or a single stock whose country
    /// and sector are known. None: not looked through (never guessed).
    pub fn look(&self) -> Option<Look> {
        if let Some(i) = instrument(&self.symbol) {
            return Some(Look {
                index: i.index.into(),
                domicile: i.domicile.into(),
                regions: i.regions.to_vec(),
                sectors: i.sectors.to_vec(),
                top: i.top.to_vec(),
            });
        }
        if self.kind != Kind::Stock {
            return None;
        }
        let (region, country) = venue_region(self.venue)?;
        let sector = company(&self.symbol).map(|c| c.sector).or_else(|| sector_in(&self.description))?;
        Some(Look { index: "single stock".into(), domicile: country.into(), regions: vec![(region, 100.)], sectors: vec![(sector, 100.)], top: vec![] })
    }

    /// The name to show: a known company's, else the label.
    pub fn name(&self) -> String {
        company(&self.symbol).map(|c| c.name.to_string()).unwrap_or_else(|| self.label.clone())
    }
}

/// Shares of a whole: (label, HKD, %), biggest first.
pub type Split = Vec<(String, f64, f64)>;

/// A stock held directly that the funds hold too.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldTwice {
    pub symbol: String,
    pub name: String,
    pub direct_hkd: f64,
    pub in_funds_hkd: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Exposure {
    pub total_hkd: f64,
    pub by_currency: Split,
    pub by_venue: Split,
    pub by_kind: Split,
    /// look-through over what has a table entry (crypto and cash excluded)
    pub by_region: Split,
    pub by_sector: Split,
    /// HKD the look-through covers, and what it can't place (no table row)
    pub looked_through_hkd: f64,
    pub unknown_hkd: f64,
    /// single stocks held directly and inside the funds, the biggest first
    pub held_twice: Vec<HeldTwice>,
    /// overlapping pairs held: (a, b, HKD in a + b, why)
    pub overlaps: Vec<(String, String, f64, &'static str)>,
}

fn split(m: BTreeMap<String, f64>, total: f64) -> Split {
    let mut v: Split = m.into_iter().filter(|(_, x)| x.abs() > 1e-9)
        .map(|(k, x)| (k, x, if total > 0. { x / total * 100. } else { 0. })).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

pub fn exposure(positions: &[Position]) -> Exposure {
    let total: f64 = positions.iter().map(|p| p.value_hkd).sum();
    let (mut ccy, mut venue, mut kind) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let (mut region, mut sector) = (BTreeMap::new(), BTreeMap::new());
    let mut e = Exposure { total_hkd: total, ..Default::default() };
    // stock symbol -> (name, direct, inside the funds)
    let mut names: BTreeMap<String, (String, f64, f64)> = BTreeMap::new();
    for p in positions.iter().filter(|p| p.kind == Kind::Stock) {
        names.entry(p.symbol.to_uppercase()).or_insert((p.name(), 0., 0.)).1 += p.value_hkd;
    }
    for p in positions {
        *ccy.entry(p.currency.clone()).or_insert(0.) += p.value_hkd;
        *venue.entry(p.venue.label().to_string()).or_insert(0.) += p.value_hkd;
        *kind.entry(p.kind.label().to_string()).or_insert(0.) += p.value_hkd;
        match (p.kind, p.look()) {
            (Kind::Crypto | Kind::Cash, _) => {}
            (_, Some(l)) => {
                e.looked_through_hkd += p.value_hkd;
                for (r, w) in &l.regions {
                    *region.entry(r.label().to_string()).or_insert(0.) += p.value_hkd * w / 100.;
                }
                for (s, w) in &l.sectors {
                    *sector.entry(s.label().to_string()).or_insert(0.) += p.value_hkd * w / 100.;
                }
                for (sym, w) in &l.top {
                    if let Some(n) = names.get_mut(*sym) {
                        n.2 += p.value_hkd * w / 100.;
                    }
                }
            }
            (_, None) => e.unknown_hkd += p.value_hkd,
        }
    }
    e.held_twice = names.into_iter().filter(|(_, n)| n.1 > 0. && n.2 > 0.)
        .map(|(symbol, (name, direct_hkd, in_funds_hkd))| HeldTwice { symbol, name, direct_hkd, in_funds_hkd }).collect();
    e.held_twice.sort_by(|a, b| (b.direct_hkd + b.in_funds_hkd).total_cmp(&(a.direct_hkd + a.in_funds_hkd)));
    let held = |s: &str| positions.iter().filter(|p| p.symbol.eq_ignore_ascii_case(s)).map(|p| p.value_hkd).sum::<f64>();
    for (a, b, why) in OVERLAPS {
        let (va, vb) = (held(a), held(b));
        if va > 0. && vb > 0. {
            e.overlaps.push((a.to_string(), b.to_string(), va + vb, why));
        }
    }
    e.by_currency = split(ccy, total);
    e.by_venue = split(venue, total);
    e.by_kind = split(kind, total);
    let lt = e.looked_through_hkd;
    e.by_region = split(region, lt);
    e.by_sector = split(sector, lt);
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_adds_up() {
        for i in INSTRUMENTS {
            let r: f64 = i.regions.iter().map(|x| x.1).sum();
            let s: f64 = i.sectors.iter().map(|x| x.1).sum();
            assert!((r - 100.).abs() < 1e-9, "{} regions sum to {r}", i.symbol);
            assert!((s - 100.).abs() < 1e-9, "{} sectors sum to {s}", i.symbol);
            for (sym, _) in i.top {
                assert!(company(sym).is_some(), "{sym} in {}'s top has a company row", i.symbol);
            }
        }
    }

    #[test]
    fn venues() {
        assert_eq!(venue_of("VWRA", "USD"), Venue::Lse); // a USD line in London
        assert_eq!(venue_of("EQQQ", "GBP"), Venue::Lse);
        assert_eq!(venue_of("XYZ", "GBP"), Venue::Lse);
        assert_eq!(venue_of("XYZ", "USD"), Venue::Us);
        assert_eq!(venue_of("XYZ", "EUR"), Venue::Unknown);
        assert_eq!(domicile_of("VWRA", "", "USD"), Some("Ireland"));
        assert_eq!(domicile_of("MSFT", "", "USD"), Some("United States"));
        assert_eq!((domicile_of("ABC", "ABC CORP", "USD"), domicile_of("ABC", "ABC CORP", "EUR")), (Some("United States"), None));
        assert_eq!(domicile_of("IB01", "", "USD"), None, "a fund the table doesn't know");
    }

    #[test]
    fn single_stocks_are_themselves() {
        let p = Position::broker("ACME", "ACME AEROSPACE CORP-CL A", "USD", 10.);
        assert_eq!(p.kind, Kind::Stock);
        let l = p.look().unwrap();
        assert_eq!((l.regions, l.sectors, l.domicile.as_str()), (vec![(Region::Us, 100.)], vec![(Sector::Industrials, 100.)], "United States"));
        assert_eq!(Position::broker("MSFT", "", "USD", 1.).look().unwrap().sectors, vec![(Sector::Tech, 100.)], "a known company");
        assert_eq!(Position::broker("ACME", "ACME CORP", "USD", 1.).look(), None, "no sector: not guessed");
    }

    #[test]
    fn look_through_and_overlaps() {
        let ps = vec![
            Position::broker("VWRA", "", "USD", 600.),
            Position::broker("VT", "", "USD", 200.),
            Position::broker("AAPL", "APPLE INC", "USD", 100.),
            Position::broker("ABC", "", "USD", 100.), // not in the table
            Position { symbol: "BTC".into(), label: "BTC".into(), description: String::new(), currency: "USD".into(), value_hkd: 500., kind: Kind::Crypto, venue: Venue::CryptoVenue },
            Position { symbol: "CASH".into(), label: "Cash".into(), description: String::new(), currency: "EUR".into(), value_hkd: 500., kind: Kind::Cash, venue: Venue::Account },
        ];
        let e = exposure(&ps);
        assert_eq!(e.total_hkd, 2000.);
        assert_eq!(e.by_currency[0], ("USD".to_string(), 1500., 75.));
        assert_eq!(e.looked_through_hkd, 900.);
        assert_eq!(e.unknown_hkd, 100.);
        // US: 600*.62 + 200*.62 + 100 = 596 of 900
        let us = e.by_region.iter().find(|r| r.0 == "United States").unwrap();
        assert!((us.1 - 596.).abs() < 1e-9 && (us.2 - 596. / 9.).abs() < 1e-9);
        assert_eq!(e.held_twice.len(), 1);
        let a = &e.held_twice[0];
        assert_eq!((a.symbol.as_str(), a.name.as_str(), a.direct_hkd), ("AAPL", "Apple", 100.));
        assert!((a.in_funds_hkd - (600. * 0.036 + 200. * 0.032)).abs() < 1e-9);
        assert_eq!(e.overlaps.len(), 1);
        assert_eq!((e.overlaps[0].0.as_str(), e.overlaps[0].2), ("VWRA", 800.));
    }
}
