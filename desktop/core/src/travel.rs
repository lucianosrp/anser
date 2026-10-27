//! Trip discovery: the stretches of days the owner spent away from home, and
//! what each one cost. Nothing is tagged by hand; trips fall out of where the
//! money was spent.
//!
//! * **Where a row was spent.** Card rows end in the merchant's country code,
//!   and, when charged in a foreign currency, the original amount
//!   ("MATSUZAKAYA NAGOYA JP JPY 780"). Only those with a foreign amount
//!   count as being somewhere: a HKD-billed foreign merchant ("UBER *TRIP
//!   HELP.UBER.COM NL", an airline) is an online purchase. Bank rows are placed
//!   by their account's country when it has one (Settings → Sources; the
//!   source catalog's default, e.g. an HSBC Hong Kong account), else by
//!   their account currency (a Revolut JPY row was spent in Japan). A card
//!   row billed in the base currency with no foreign amount counts only in
//!   the card's own country. EUR accounts are weak evidence: their cards get
//!   used from home too (DiDi, a cinema, PayPal), so EUR rows alone make a
//!   trip only when there are several days of them. ([`crate::base`] holds
//!   the base currency, the home country and the account countries.)
//! * **Home moves.** It is the place with the most spending days in the
//!   surrounding ±60 days, so after a move abroad the new country is home and
//!   a visit back to the old one counts as away. On a tie the profile's
//!   home country wins.
//! * **Trips.** Away days at most [`GAP_DAYS`] apart form one trip (up to
//!   [`QUIET_GAP_DAYS`] when nothing was spent at home in between); a lone
//!   foreign purchase is not a trip. A merchant away in 3+ different months
//!   is an online service (an API bill in USD), not travel. Fees, transfers
//!   and rows without a description place nobody.
//! * **Cost.** Spending abroad during the trip, plus the trip's bookings:
//!   Travel-category rows (flights, hotels, eSIMs) paid in the
//!   [`BOOKED_AHEAD_DAYS`] before it or from home while away.
//! * **Routes** ([`route`], [`atlas`]). Where in a trip each row was: a city
//!   the description names ([`geo::find_city`], within the row's country),
//!   a country snapped to the trip's nearest named city there, or, for a row
//!   naming nothing, where the owner was that day. Each day goes to the place
//!   most of its rows point to; runs of days are [`Stop`]s; home -> stops ->
//!   home are [`Leg`]s, solid when a ticket or an airport receipt backs them.
//! * **Transport** ([`transport`]): flights, trains, buses, boats, rides,
//!   local transit, stays, and bookings that could be any of them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{Datelike, NaiveDate};

use crate::drill::{is_spending, merchant_key, my_hkd};
use crate::geo::{self, LonLat};
use crate::ledger::{Entry, Kind, Ledger};

/// Away days this far apart still belong to one trip (quiet days with no
/// foreign purchase, or ones paid in cash)...
pub const GAP_DAYS: i64 = 3;
/// ...and up to this far apart when nothing was spent at home in between.
pub const QUIET_GAP_DAYS: i64 = 14;
/// A booking this many days before a trip is counted toward it.
pub const BOOKED_AHEAD_DAYS: i64 = 120;
/// A trip made of weak (EUR account) rows alone needs this many away days
/// and rows.
const WEAK_MIN_DAYS: usize = 3;
const WEAK_MIN_ROWS: usize = 5;
/// Half-width of the window that decides where home is.
const HOME_WINDOW_DAYS: i64 = 60;

// Categories paid wherever the owner happens to be (direct debits,
// subscriptions) place nobody anywhere: the category flag
// categories::placeless.
/// Description starts that are money sent, not spent somewhere.
const NOT_A_PLACE: [&str; 5] = ["to ", "transfer", "payment from", "paypal", "exchanged"];

#[derive(Debug, Clone, PartialEq)]
pub struct Trip {
    /// "YYYY-MM-DD", first and last away day
    pub start: String,
    pub end: String,
    /// destinations (display names), biggest spend first
    pub places: Vec<String>,
    /// where home was ("HK", or "EU" for the euro area): the zone code
    pub home: String,
    /// spending abroad during the trip (ledger indices)
    pub away_rows: Vec<usize>,
    /// travel bookings paid ahead or from home (ledger indices)
    pub booked_rows: Vec<usize>,
    /// HKD at the owner's share, positive = spent
    pub away_hkd: f64,
    pub booked_hkd: f64,
    /// spend by category, biggest first (bookings included)
    pub by_category: Vec<(String, f64)>,
}

impl Trip {
    pub fn days(&self) -> i64 {
        match (day(&self.start), day(&self.end)) {
            (Some(a), Some(b)) => (b - a).num_days() + 1,
            _ => 1,
        }
    }

    pub fn total_hkd(&self) -> f64 {
        self.away_hkd + self.booked_hkd
    }

    /// Spending per day on the ground (bookings excluded: a flight isn't
    /// spread over the stay).
    pub fn per_day_hkd(&self) -> f64 {
        self.away_hkd / self.days() as f64
    }

    /// "Japan", "Italy · Europe"
    pub fn label(&self) -> String {
        if self.places.is_empty() { "Abroad".into() } else { self.places.join(" · ") }
    }

    /// Every row of the trip (ledger indices, in ledger order).
    pub fn rows(&self) -> Vec<usize> {
        let mut all: Vec<usize> = self.away_rows.iter().chain(&self.booked_rows).copied().collect();
        all.sort_unstable();
        all
    }
}

fn day(date: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(date.get(..10)?, "%Y-%m-%d").ok()
}

/// Days since the common era: a plain integer to measure gaps with.
fn day_num(date: &str) -> Option<i64> {
    day(date).map(|d| d.num_days_from_ce() as i64)
}

/// Where a spending row places the owner: (zone, place). The zone decides
/// home vs away (the euro area is one zone: a trip from Rome to Paris is
/// not travel when Europe is home), the place names the destination.
/// The third value says whether the evidence is strong (see the module doc).
fn place_of(e: &Entry) -> Option<(String, String, bool)> {
    if !is_spending(e) {
        return None;
    }
    let desc = e.description.trim().to_lowercase();
    if crate::categories::placeless(&e.category) || desc.is_empty() || NOT_A_PLACE.iter().any(|p| desc.starts_with(p)) {
        return None;
    }
    let home = crate::base::get();
    let (cc, strong) = match e.kind {
        Kind::Card => (card_place(&e.description, &home.currency, card_country(&home, &e.source).as_deref())?, true),
        Kind::Bank => match home.account_country(&e.source) {
            Some(cc) => (cc, true),
            None => {
                let cc = currency_place(&e.currency)?;
                let strong = cc != "EU";
                (cc, strong)
            }
        },
        _ => return None,
    };
    Some((zone(&cc).to_string(), cc, strong))
}

/// The country a card is used from at home: its account's, else the
/// profile's home country.
fn card_country(home: &crate::base::Home, source: &str) -> Option<String> {
    home.account_country(source).or_else(|| home.country.clone())
}

/// "… NAGOYA JP JPY 780" -> "JP"; "… KOWLOON HK" -> "HK" (a Hong
/// Kong card, base HKD); a foreign country without a foreign amount (billed
/// in the base currency, so bought online) -> None.
fn card_place(desc: &str, base: &str, card_cc: Option<&str>) -> Option<String> {
    let words: Vec<&str> = desc.split_whitespace().collect();
    let is_code = |w: &str, n: usize| w.len() == n && w.chars().all(|c| c.is_ascii_uppercase());
    let is_amount = |w: &str| w.replace(',', "").parse::<f64>().is_ok();
    match words.as_slice() {
        [.., cc, ccy, amt] if is_code(cc, 2) && is_code(ccy, 3) && is_amount(amt) => {
            (*ccy != base).then(|| cc.to_string())
        }
        [.., cc] if is_code(cc, 2) => (Some(*cc) == card_cc).then(|| cc.to_string()),
        _ => None,
    }
}

/// An account currency's country: the ISO 4217 code starts with the ISO 3166
/// one (JPY -> JP, MOP -> MO, GBP -> GB); EUR is the euro area.
fn currency_place(ccy: &str) -> Option<String> {
    match ccy {
        "EUR" => Some("EU".into()),
        c if c.len() == 3 && c.chars().all(|c| c.is_ascii_uppercase()) => Some(c[..2].to_string()),
        _ => None,
    }
}

const EURO_AREA: [&str; 20] = [
    "AT", "BE", "HR", "CY", "EE", "FI", "FR", "DE", "GR", "IE", "IT", "LV", "LT", "LU", "MT", "NL", "PT", "SK", "SI", "ES",
];

fn zone(cc: &str) -> &str {
    if EURO_AREA.contains(&cc) { "EU" } else { cc }
}

/// Display name for a place code.
pub fn place_name(cc: &str) -> String {
    let name = match cc {
        "EU" => "Europe",
        "HK" => "Hong Kong",
        "MO" => "Macau",
        "CN" => "China",
        "TW" => "Taiwan",
        "JP" => "Japan",
        "KR" => "South Korea",
        "TH" => "Thailand",
        "VN" => "Vietnam",
        "SG" => "Singapore",
        "MY" => "Malaysia",
        "ID" => "Indonesia",
        "PH" => "Philippines",
        "IN" => "India",
        "AU" => "Australia",
        "NZ" => "New Zealand",
        "US" => "United States",
        "CA" => "Canada",
        "MX" => "Mexico",
        "GB" => "United Kingdom",
        "CH" => "Switzerland",
        "NO" => "Norway",
        "SE" => "Sweden",
        "DK" => "Denmark",
        "PL" => "Poland",
        "CZ" => "Czechia",
        "HU" => "Hungary",
        "TR" => "Türkiye",
        "AE" => "UAE",
        "IT" => "Italy",
        "FR" => "France",
        "DE" => "Germany",
        "ES" => "Spain",
        "PT" => "Portugal",
        "NL" => "Netherlands",
        "BE" => "Belgium",
        "AT" => "Austria",
        "IE" => "Ireland",
        "GR" => "Greece",
        "FI" => "Finland",
        "HR" => "Croatia",
        other => return other.to_string(),
    };
    name.to_string()
}

struct Signal {
    day: i64,
    zone: String,
    place: String,
    strong: bool,
    row: usize,
}

/// Every trip in the ledger, oldest first.
pub fn discover(ledger: &Ledger) -> Vec<Trip> {
    let signals: Vec<Signal> = ledger.entries.iter().enumerate()
        .filter_map(|(row, e)| {
            let (zone, place, strong) = place_of(e)?;
            Some(Signal { day: day_num(&e.date)?, zone, place, strong, row })
        })
        .collect();

    // spending days per zone, for "where is home around day d"
    let mut zone_days: HashMap<&str, Vec<i64>> = HashMap::new();
    for s in &signals {
        zone_days.entry(&s.zone).or_default().push(s.day);
    }
    for days in zone_days.values_mut() {
        days.sort_unstable();
        days.dedup();
    }
    // the profile's home country breaks a tie
    let prior: Option<String> = crate::base::get().country.as_deref().map(|c| zone(c).to_string());
    let is_prior = |z: &str| prior.as_deref() == Some(z);
    let home_at = |d: i64| -> &str {
        let in_window = |days: &Vec<i64>| {
            days.partition_point(|&x| x <= d + HOME_WINDOW_DAYS) - days.partition_point(|&x| x < d - HOME_WINDOW_DAYS)
        };
        zone_days.iter()
            .max_by(|a, b| in_window(a.1).cmp(&in_window(b.1)).then(is_prior(a.0).cmp(&is_prior(b.0))).then(b.0.cmp(a.0)))
            .map_or("", |(z, _)| z)
    };
    let (mut away, home): (Vec<&Signal>, Vec<&Signal>) = signals.iter().partition(|s| s.zone != home_at(s.day));
    let mut home_days: Vec<i64> = home.iter().map(|s| s.day).collect();
    home_days.sort_unstable();
    home_days.dedup();
    let home_between = |a: i64, b: i64| home_days.partition_point(|&x| x < b) > home_days.partition_point(|&x| x <= a);

    // online services: away in 3+ separate months is a subscription, not travel
    let mut months_by_merchant: HashMap<String, BTreeSet<String>> = HashMap::new();
    for s in &away {
        let e = &ledger.entries[s.row];
        months_by_merchant.entry(merchant_key(&e.description)).or_default().insert(e.date[..7].to_string());
    }
    away.retain(|s| {
        let key = merchant_key(&ledger.entries[s.row].description);
        key.is_empty() || months_by_merchant.get(&key).is_none_or(|m| m.len() < 3)
    });
    away.sort_by_key(|s| (s.day, s.row));

    // cluster away days into trips
    let mut groups: Vec<Vec<&Signal>> = Vec::new();
    for s in away {
        match groups.last_mut() {
            Some(g) if {
                let last = g.last().map_or(s.day, |l| l.day);
                s.day - last <= GAP_DAYS || (s.day - last <= QUIET_GAP_DAYS && !home_between(last, s.day))
            } => g.push(s),
            _ => groups.push(vec![s]),
        }
    }
    // a lone purchase is not a trip, nor a few weak rows; neither are the
    // last weeks before a move (home is a window majority, so it flips a
    // little early): away to where home was two months before, or will be
    // two months after
    groups.retain(|g| {
        let (first, last) = (g[0], g[g.len() - 1]);
        let days = g.iter().map(|s| s.day).collect::<BTreeSet<_>>().len();
        let evidence = g.iter().any(|s| s.strong) || (days >= WEAK_MIN_DAYS && g.len() >= WEAK_MIN_ROWS);
        g.len() >= 2 && evidence
            && home_at(first.day - 2 * HOME_WINDOW_DAYS) != first.zone
            && home_at(last.day + 2 * HOME_WINDOW_DAYS) != last.zone
    });

    let hkd = |row: usize| -my_hkd(&ledger.entries[row]).unwrap_or(0.);
    let mut trips: Vec<Trip> = groups.iter().map(|g| {
        let mut by_place: HashMap<&str, f64> = HashMap::new();
        for s in g {
            *by_place.entry(&s.place).or_default() += hkd(s.row);
        }
        let mut places: Vec<(&str, f64)> = by_place.into_iter().collect();
        places.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
        // "Europe" (a EUR account row) says less than any named country: a
        // EUR card in Japan was spent in Japan
        let named = places.iter().any(|(p, _)| *p != "EU");
        let names: Vec<String> = places.iter().filter(|(p, _)| !(named && *p == "EU")).map(|(p, _)| place_name(p)).collect();
        let rows: Vec<usize> = g.iter().map(|s| s.row).collect();
        Trip {
            start: ledger.entries[g[0].row].date[..10].to_string(),
            end: ledger.entries[g[g.len() - 1].row].date[..10].to_string(),
            places: names,
            home: home_at(g[0].day).to_string(),
            away_hkd: rows.iter().map(|&r| hkd(r)).sum(),
            away_rows: rows,
            booked_rows: Vec::new(),
            booked_hkd: 0.,
            by_category: Vec::new(),
        }
    }).collect();

    // card DCC fees ride along with the foreign purchase they were charged on
    let card_days: Vec<BTreeSet<&str>> = trips.iter().map(|t| {
        t.away_rows.iter().filter(|&&r| ledger.entries[r].kind == Kind::Card).map(|&r| &ledger.entries[r].date[..10]).collect()
    }).collect();
    // bookings: Travel rows not already spent abroad, to the trip they precede or fall in
    let taken: BTreeSet<usize> = trips.iter().flat_map(|t| t.away_rows.iter().copied()).collect();
    for (row, e) in ledger.entries.iter().enumerate() {
        if taken.contains(&row) || !is_spending(e) {
            continue;
        }
        let Some(d) = day_num(&e.date) else { continue };
        let travel = e.category.eq_ignore_ascii_case("travel");
        let dcc = e.kind == Kind::Card && e.description.to_uppercase().starts_with("DCC FEE");
        let hit = trips.iter().enumerate().find(|(i, t)| {
            let (Some(s), Some(end)) = (day_num(&t.start), day_num(&t.end)) else { return false };
            (travel && d >= s - BOOKED_AHEAD_DAYS && d <= end)
                || (dcc && card_days[*i].contains(&e.date[..10]))
        });
        if let Some((i, _)) = hit {
            trips[i].booked_rows.push(row);
            trips[i].booked_hkd += hkd(row);
        }
    }
    for t in &mut trips {
        let mut cats: BTreeMap<&str, f64> = BTreeMap::new();
        for r in t.away_rows.iter().chain(&t.booked_rows) {
            *cats.entry(ledger.entries[*r].category.as_str()).or_default() += hkd(*r);
        }
        let mut cats: Vec<(String, f64)> = cats.into_iter().map(|(c, v)| (c.to_string(), v)).collect();
        cats.sort_by(|a, b| b.1.total_cmp(&a.1));
        t.by_category = cats;
    }
    trips
}

// ---------------------------------------------------------------------------
// Transport and routes: how the owner moved during a trip, for the map.

/// How a trip row moved (or housed) the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mode {
    Flight,
    Train,
    Bus,
    Boat,
    /// Uber, DiDi, taxis
    Ride,
    /// metro, monorail, city transit cards
    Metro,
    /// hotels, Airbnb, Booking.com: a stay, not a leg
    Stay,
    /// trip.com, Klook: could be a flight, a hotel or a ticket
    Booking,
}

impl Mode {
    pub const ALL: [Mode; 8] = [Mode::Flight, Mode::Train, Mode::Bus, Mode::Boat, Mode::Ride, Mode::Metro, Mode::Stay, Mode::Booking];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Flight => "Flight",
            Mode::Train => "Train",
            Mode::Bus => "Bus",
            Mode::Boat => "Boat",
            Mode::Ride => "Ride",
            Mode::Metro => "Local transit",
            Mode::Stay => "Stay",
            Mode::Booking => "Booking",
        }
    }

    /// "2 flights", "1 stay", "3 local"
    pub fn count(self, n: usize) -> String {
        let (one, many) = match self {
            Mode::Flight => ("flight", "flights"),
            Mode::Train => ("train", "trains"),
            Mode::Bus => ("bus", "buses"),
            Mode::Boat => ("boat", "boats"),
            Mode::Ride => ("ride", "rides"),
            Mode::Metro => ("local", "local"),
            Mode::Stay => ("stay", "stays"),
            Mode::Booking => ("booking", "bookings"),
        };
        format!("{n} {}", if n == 1 { one } else { many })
    }

    /// Takes the owner from one place to another (a leg on the map).
    pub fn is_leg(self) -> bool {
        matches!(self, Mode::Flight | Mode::Train | Mode::Bus | Mode::Boat)
    }
}

/// `kw` starts a word of `folded` (both lowercase, spaces between words):
/// "swiss" finds "swiss7241234567890" and "air france", not "unswiss".
fn has_word(folded: &str, kw: &str) -> bool {
    folded.match_indices(kw).any(|(i, _)| i == 0 || folded.as_bytes()[i - 1] == b' ')
}

const BOAT: [&str; 12] = ["chu kong", "ferry", "ferries", "turbojet", "cotai water", "traghett", "caronte", "liberty lines",
    "hovercraft", "siremar", "tirrenia", "grimaldi"];
/// matched anywhere in a word ("Parkhotel")
const STAY_IN: [&str; 4] = ["hotel", "hostel", "swissotel", "motel"];
const STAY: [&str; 6] = ["airbnb", "booking com", "b and b", "b b hot", "ryokan", "agoda"];
const FLIGHT: [&str; 30] = ["air france", "airfrance", "ita airways", "turkish airlines", "turkishairlines", "finnair",
    "lufthansa", "deutsche lufthansa", "easyjet", "ryanair", "transavia", "aeroitalia", "norwegian", "klm", "british airways",
    "cathay", "hk express", "hkexpress", "greater bay airlines", "peach aviation", "jetstar", "vueling", "wizz", "volotea",
    "emirates", "qatar airways", "airasia", "japan airlines", "all nippon", "korean air"];
const FLIGHT_IN: [&str; 2] = ["airlines", "airways"];
const TRAIN: [&str; 18] = ["sncf", "trenitalia", "italo", "keisei", "japan rail", "eurostar", "sbb", "deutsche bahn",
    "db fernverkehr", "db vertrieb", "trainline", "thetrainline", "renfe", "ouigo", "thalys", "shinkansen", "obb", "railway"];
const BUS: [&str; 9] = ["flixbus", "autolinee", "interbus", "national express", "megabus", "etna trasporti", "etna transporti",
    "busitalia", "autobus"];
const RIDE: [&str; 10] = ["uber", "ubr", "didi", "taxi", "freenow", "free now", "bolt", "grab", "lyft", "cabify"];
const METRO_IN: [&str; 2] = ["monorail", "metropolitana"];
const METRO: [&str; 17] = ["ile de france mobilites", "mobilites", "navigo", "ratp", "octopus", "tokyo metro", "metro", "tfl",
    "ovpay", "gvb", "logpay", "suica", "pasmo", "mtr", "atac", "tram", "u bahn"];
const BOOKING: [&str; 7] = ["trip com", "klook", "expedia", "kayak", "opodo", "edreams", "lastminute"];

/// Classify a row as transport or a stay, by its description (the keywords
/// of the default Travel and Transport rules, plus local transit seen
/// abroad). trip.com stays a booking: it sells flights and hotels alike.
pub fn transport(description: &str) -> Option<Mode> {
    let f = crate::geo::fold(description);
    let word = |list: &[&str]| list.iter().any(|kw| has_word(&f, kw));
    let inside = |list: &[&str]| list.iter().any(|kw| f.contains(kw));
    if has_word(&f, "uber eats") {
        return None; // food, not a ride
    }
    let words: Vec<&str> = f.split(' ').collect();
    let exact = |w: &str| words.contains(&w);
    // SWISS tickets read "SWISS7241234567891"; Swissquote is a broker
    let swiss = words.iter().any(|w| w.strip_prefix("swiss").is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit())));
    if word(&BOAT) {
        Some(Mode::Boat)
    } else if inside(&STAY_IN) || word(&STAY) {
        Some(Mode::Stay)
    } else if word(&FLIGHT) || inside(&FLIGHT_IN) || swiss {
        Some(Mode::Flight)
    } else if word(&TRAIN) || exact("jr") {
        Some(Mode::Train)
    } else if word(&BUS) || exact("bus") {
        Some(Mode::Bus)
    } else if word(&RIDE) {
        Some(Mode::Ride)
    } else if inside(&METRO_IN) || word(&METRO) {
        Some(Mode::Metro)
    } else if word(&BOOKING) {
        Some(Mode::Booking)
    } else {
        None
    }
}

/// Merchants that say where they are better than their description's words
/// (folded keyword, city, country).
const PLACE_HINTS: [(&str, &str, &str); 14] = [
    ("ile de france mobilites", "Paris", "FR"),
    ("navigo", "Paris", "FR"),
    ("ratp", "Paris", "FR"),
    ("tfl", "London", "GB"),
    ("ovpay", "Amsterdam", "NL"),
    ("gvb", "Amsterdam", "NL"),
    ("schiphol", "Amsterdam", "NL"),
    ("keisei", "Tokyo", "JP"),
    ("tokyo metro", "Tokyo", "JP"),
    ("okinawatoshimonorail", "Okinawa", "JP"),
    ("okinawa monorail", "Okinawa", "JP"),
    ("etna trasporti", "Catania", "IT"),
    ("etna transporti", "Catania", "IT"),
    ("azienda metropolitana", "Catania", "IT"),
];
/// ...and merchants that only name a country.
const COUNTRY_HINTS: [(&str, &str); 9] = [
    ("sncf", "FR"), ("trenitalia", "IT"), ("sbb", "CH"), ("jumbo supermarkt", "NL"), ("albert heijn", "NL"),
    ("edeka", "DE"), ("rossmann", "DE"), ("monoprix", "FR"), ("e leclerc", "FR"),
];

/// A place on the route.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    /// ISO country ("EU" when only the euro area is known)
    pub cc: String,
    pub at: LonLat,
}

impl Place {
    fn city(c: &crate::geo::City) -> Place {
        Place { name: c.name.clone(), cc: c.cc.clone(), at: c.at }
    }
    fn country(cc: &str) -> Option<Place> {
        Some(Place { name: place_name(cc), cc: cc.to_string(), at: geo::country_point(cc)? })
    }
}

/// Where a row says it was spent.
enum Loc {
    City(Place),
    Country(String),
    Unknown,
}

fn locate(e: &Entry, extra: &[String]) -> Loc {
    let f = geo::fold(&e.description);
    if let Some((_, city, cc)) = PLACE_HINTS.iter().find(|(kw, _, _)| has_word(&f, kw)) {
        if let Some(c) = geo::city(city, cc) {
            return Loc::City(Place::city(c));
        }
    }
    let known_cc = match e.kind {
        Kind::Card => {
            let home = crate::base::get();
            card_place(&e.description, &home.currency, card_country(&home, &e.source).as_deref())
        }
        Kind::Bank => currency_place(&e.currency).filter(|c| c != "EU"),
        _ => None,
    };
    // a flight or a bus company names its seat, not where the owner was
    // (a card row's city is the terminal's, so it stands)
    let names_hq = e.kind != Kind::Card && matches!(transport(&e.description), Some(Mode::Flight | Mode::Bus | Mode::Booking));
    if !names_hq {
        let mut allowed: Vec<&str> = match &known_cc {
            Some(cc) => vec![cc.as_str()],
            None => geo::EUROPE.to_vec(),
        };
        if known_cc.is_none() {
            allowed.extend(extra.iter().map(String::as_str));
        }
        if let Some(c) = geo::find_city(&e.description, &allowed) {
            return Loc::City(Place::city(c));
        }
    }
    if let Some(cc) = known_cc {
        return Loc::Country(cc);
    }
    match COUNTRY_HINTS.iter().find(|(kw, _)| has_word(&f, kw)) {
        Some((_, cc)) => Loc::Country(cc.to_string()),
        None => Loc::Unknown,
    }
}

/// A stretch of the trip spent in one place.
#[derive(Debug, Clone)]
pub struct Stop {
    pub place: Place,
    /// first and last day with spending there
    pub first: String,
    pub last: String,
    /// HKD at the owner's share, positive = spent
    pub hkd: f64,
    pub rows: Vec<usize>,
    /// placed only by neighbouring rows, nothing named it
    pub inferred: bool,
}

/// A move between two points of the route (home first and last).
#[derive(Debug, Clone)]
pub struct Leg {
    pub from: Place,
    pub to: Place,
    /// None: no way to tell (short hops with no ticket)
    pub mode: Option<Mode>,
    /// no ticket or airport row backs it: drawn dashed
    pub inferred: bool,
}

/// A transport or stay row of the trip.
#[derive(Debug, Clone)]
pub struct Transit {
    pub row: usize,
    pub mode: Mode,
    pub hkd: f64,
}

#[derive(Debug, Clone)]
pub struct Route {
    pub home: Place,
    pub stops: Vec<Stop>,
    /// home -> stops in date order -> home
    pub legs: Vec<Leg>,
    /// transport and stays, by date
    pub transit: Vec<Transit>,
    /// (mode, rows, HKD) in [`Mode::ALL`] order
    pub by_mode: Vec<(Mode, usize, f64)>,
    /// days with spending per country (ISO), most first
    pub country_days: Vec<(String, usize)>,
}

/// Every trip's route, aligned with the trips.
#[derive(Debug, Clone, Default)]
pub struct Atlas {
    pub routes: Vec<Route>,
}

/// Somewhere in the euro area, when nothing narrows it down.
fn europe() -> Place {
    Place { name: "Europe".into(), cc: "EU".into(), at: geo::country_point("EU").unwrap_or(LonLat { lon: 10.0, lat: 48.5 }) }
}

pub fn atlas(ledger: &Ledger, trips: &[Trip]) -> Atlas {
    Atlas { routes: trips.iter().map(|t| route(ledger, t)).collect() }
}

/// Where home is drawn: the profile's home city when home is its country's
/// zone, a city-state's city, else the country (Europe for the euro area).
fn home_place(zone_code: &str) -> Place {
    let home = crate::base::get();
    let mine = home
        .country
        .as_deref()
        .filter(|c| zone(c) == zone_code)
        .and_then(|c| home.city.as_deref().and_then(|city| geo::city(city, c)))
        .map(Place::city);
    mine.or_else(|| match zone_code {
        "EU" => None,
        cc @ ("HK" | "MO" | "SG") => geo::city(&place_name(cc), cc).map(Place::city),
        cc => Place::country(cc),
    })
    .unwrap_or_else(europe)
}

/// A trip's stops, legs and transport.
pub fn route(ledger: &Ledger, trip: &Trip) -> Route {
    let hkd = |row: usize| -my_hkd(&ledger.entries[row]).unwrap_or(0.);
    let home = home_place(&trip.home);
    let mut rows = trip.away_rows.clone();
    rows.sort_by_key(|&r| (ledger.entries[r].date.get(..10).map(String::from), r));
    // countries the trip has strong evidence for (biggest spend first): a
    // EUR card row may name a city there too ("Tokyo Metro" paid from a EUR
    // account), and a row that names nothing falls back to the first
    let mut strong: HashMap<String, f64> = HashMap::new();
    for &r in &rows {
        if let Some((_, cc, true)) = place_of(&ledger.entries[r]) {
            if cc != "EU" {
                *strong.entry(cc).or_default() += hkd(r);
            }
        }
    }
    let mut extra: Vec<(String, f64)> = strong.into_iter().collect();
    extra.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let extra: Vec<String> = extra.into_iter().map(|(c, _)| c).collect();

    let days: Vec<i64> = rows.iter().map(|&r| day_num(&ledger.entries[r].date).unwrap_or(0)).collect();
    let locs: Vec<Loc> = rows.iter().map(|&r| locate(&ledger.entries[r], &extra)).collect();
    let nearest = |k: usize, ok: &dyn Fn(usize) -> bool| -> Option<usize> {
        (0..rows.len()).filter(|&j| j != k && ok(j)).min_by_key(|&j| ((days[j] - days[k]).abs(), j))
    };
    let city_in = |k: usize, cc: &str| -> Option<Place> {
        match nearest(k, &|j| matches!(&locs[j], Loc::City(p) if p.cc == cc)).map(|j| &locs[j]) {
            Some(Loc::City(p)) => Some(p.clone()),
            _ => Place::country(cc),
        }
    };
    // where each row was, how much it counts (a named city 1, a named
    // country 0.7, snapped to the trip's nearest city there) and whether
    // the row itself said so
    let mut placed: Vec<Option<(Place, f64, bool)>> = (0..rows.len()).map(|k| match &locs[k] {
        Loc::City(p) => Some((p.clone(), 1., true)),
        Loc::Country(cc) => city_in(k, cc).map(|p| (p, 0.7, true)),
        Loc::Unknown => None,
    }).collect();
    // rows that name nothing (a EUR card) were where the owner was that day
    // or the next (by a row that isn't a late-posting transit card); failing
    // that, in the trip's main country; failing that, at the nearest place
    // the trip names at all
    let transit_card = |k: usize| matches!(transport(&ledger.entries[rows[k]].description), Some(Mode::Metro | Mode::Ride));
    let mut guesses = vec![None; rows.len()];
    for k in 0..rows.len() {
        if placed[k].is_some() {
            continue;
        }
        let near = nearest(k, &|j| !matches!(locs[j], Loc::Unknown) && !transit_card(j)).filter(|&j| (days[j] - days[k]).abs() <= 2);
        guesses[k] = match near.and_then(|j| placed[j].clone()) {
            Some((p, _, _)) => Some(p),
            None => extra.first().and_then(|cc| city_in(k, cc)),
        };
    }
    for k in 0..rows.len() {
        if placed[k].is_none() {
            let guess = guesses[k].take()
                .or_else(|| nearest(k, &|j| placed[j].is_some()).and_then(|j| placed[j].as_ref().map(|p| p.0.clone())))
                .unwrap_or_else(europe);
            placed[k] = Some((guess, 0.3, false));
        }
    }
    // transit cards post a day or two late (OVpay, the Paris metro), so
    // they count less than a meal or a shop when a day is split
    for (k, &r) in rows.iter().enumerate() {
        if let Some((_, w, _)) = placed[k].as_mut() {
            if matches!(transport(&ledger.entries[r].description), Some(Mode::Metro | Mode::Ride)) {
                *w *= 0.4;
            }
        }
    }

    // each day was where most of its evidence says; runs of days in one
    // place are stops, and every row of those days belongs to the stop
    let mut by_day: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for k in 0..rows.len() {
        by_day.entry(days[k]).or_default().push(k);
    }
    let mut stops: Vec<Stop> = Vec::new();
    for ks in by_day.values() {
        let mut score: Vec<(&Place, f64, bool)> = Vec::new();
        for &k in ks {
            let Some((p, w, direct)) = placed[k].as_ref() else { continue };
            match score.iter_mut().find(|(q, _, _)| *q == p) {
                Some(s) => {
                    s.1 += w;
                    s.2 |= *direct;
                }
                None => score.push((p, *w, *direct)),
            }
        }
        let Some(&(place, _, direct)) = score.iter().fold(None::<&(&Place, f64, bool)>, |best, s| match best {
            Some(b) if b.1 >= s.1 => Some(b),
            _ => Some(s),
        }) else { continue };
        let date = ledger.entries[rows[ks[0]]].date[..10].to_string();
        let spent: f64 = ks.iter().map(|&k| hkd(rows[k])).sum();
        let day_rows: Vec<usize> = ks.iter().map(|&k| rows[k]).collect();
        match stops.last_mut() {
            Some(s) if &s.place == place => {
                s.last = date;
                s.hkd += spent;
                s.rows.extend(day_rows);
                s.inferred &= !direct;
            }
            _ => stops.push(Stop { place: place.clone(), first: date.clone(), last: date, hkd: spent, rows: day_rows, inferred: !direct }),
        }
    }
    // one day elsewhere between two stretches in one place, with only a row
    // or two to show for it, is noise (a head office's town), not a day trip
    let mut k = 1;
    while k + 1 < stops.len() {
        if stops[k].first == stops[k].last && stops[k].rows.len() <= 2 && stops[k - 1].place == stops[k + 1].place {
            let mid = stops.remove(k);
            let next = stops.remove(k);
            let prev = &mut stops[k - 1];
            prev.last = next.last;
            prev.hkd += mid.hkd + next.hkd;
            prev.rows.extend(mid.rows);
            prev.rows.extend(next.rows);
            prev.inferred &= next.inferred;
        } else {
            k += 1;
        }
    }

    // 4. transport rows, and what they say about the legs
    let mut all = trip.rows();
    all.sort_by_key(|&r| (ledger.entries[r].date.clone(), r));
    let transit: Vec<Transit> = all.iter()
        .filter_map(|&r| transport(&ledger.entries[r].description).map(|mode| Transit { row: r, mode, hkd: hkd(r) }))
        .collect();
    let by_mode: Vec<(Mode, usize, f64)> = Mode::ALL.iter().filter_map(|&m| {
        let of: Vec<&Transit> = transit.iter().filter(|t| t.mode == m).collect();
        (!of.is_empty()).then(|| (m, of.len(), of.iter().map(|t| t.hkd).sum()))
    }).collect();
    // dated evidence of moving: tickets, plus spending at an airport
    let evidence: Vec<(i64, Mode)> = transit.iter()
        .filter(|t| t.mode.is_leg())
        .filter_map(|t| Some((day_num(&ledger.entries[t.row].date)?, t.mode)))
        .chain(trip.away_rows.iter().filter_map(|&r| {
            let f = geo::fold(&ledger.entries[r].description);
            ["airport", "aeroport", "aeroporti", "aeroporto", "flughafen"].iter().any(|kw| has_word(&f, kw))
                .then_some(day_num(&ledger.entries[r].date)?)
                .map(|d| (d, Mode::Flight))
        }))
        .collect();
    let trip_modes: BTreeSet<Mode> = evidence.iter().map(|(_, m)| *m).collect();
    let mut points: Vec<(Place, Option<(i64, i64)>)> = vec![(home.clone(), None)];
    for s in &stops {
        points.push((s.place.clone(), Some((day_num(&s.first).unwrap_or(0), day_num(&s.last).unwrap_or(0)))));
    }
    points.push((home.clone(), None));
    let mut legs = Vec::new();
    for w in points.windows(2) {
        let ((a, a_days), (b, b_days)) = (&w[0], &w[1]);
        if a == b {
            continue;
        }
        let d = geo::km(a.at, b.at);
        let (mode, inferred) = match (a_days, b_days) {
            // between two stops: a ticket dated around the move
            (Some((_, a_last)), Some((b_first, _))) => {
                let near: BTreeSet<Mode> = evidence.iter()
                    .filter(|(day, _)| *day >= a_last - 1 && *day <= b_first + 1)
                    .map(|(_, m)| *m)
                    .collect();
                pick_mode(&near, d)
            }
            // to or from home: any long-distance ticket of the trip
            _ => pick_mode(&trip_modes, d),
        };
        legs.push(Leg { from: a.clone(), to: b.clone(), mode, inferred });
    }

    let mut country_days: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for (k, &r) in rows.iter().enumerate() {
        if let Some((p, _, _)) = &placed[k] {
            if p.cc != "EU" {
                country_days.entry(&p.cc).or_default().insert(&ledger.entries[r].date[..10]);
            }
        }
    }
    let mut country_days: Vec<(String, usize)> = country_days.into_iter().map(|(c, d)| (c.to_string(), d.len())).collect();
    country_days.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    Route { home, stops, legs, transit, by_mode, country_days }
}

/// The likeliest way a leg of `km` was travelled, given the modes seen; and
/// whether it is a guess.
fn pick_mode(seen: &BTreeSet<Mode>, km: f32) -> (Option<Mode>, bool) {
    let has = |m| seen.contains(&m);
    if km > 1000. {
        return (Some(Mode::Flight), !has(Mode::Flight));
    }
    for m in [Mode::Boat, Mode::Train, Mode::Bus] {
        if has(m) {
            return (Some(m), false);
        }
    }
    if has(Mode::Flight) && km > 300. {
        return (Some(Mode::Flight), false);
    }
    (if km > 700. { Some(Mode::Flight) } else { None }, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind, source: &str, date: &str, desc: &str, cat: &str, ccy: &str, hkd: f64) -> Entry {
        Entry {
            kind,
            date: date.into(),
            source: source.into(),
            account: String::new(),
            description: desc.into(),
            category: cat.into(),
            amount: hkd,
            currency: ccy.into(),
            hkd: Some(hkd),
            share: 1.0,
            haystack: desc.to_lowercase(), raw_hash: String::new(),
        }
    }
    fn card(date: &str, desc: &str, cat: &str, hkd: f64) -> Entry {
        entry(Kind::Card, "hsbc_card", date, desc, cat, "HKD", hkd)
    }
    fn revolut(date: &str, desc: &str, cat: &str, ccy: &str, hkd: f64) -> Entry {
        entry(Kind::Bank, "revolut_new", date, desc, cat, ccy, hkd)
    }

    /// A Hong Kong life: a coffee every few days all year, except while away.
    fn home_life(away: (&str, &str)) -> Vec<Entry> {
        let mut v = Vec::new();
        for m in 1..=12 {
            for d in [2, 6, 10, 14, 18, 22, 26] {
                let date = format!("2026-{m:02}-{d:02}");
                if !(away.0..=away.1).contains(&date.as_str()) {
                    v.push(card(&date, "DAILY GRIND COFFEE HK", "Dining", -35.));
                }
            }
        }
        v
    }
    const NEVER: (&str, &str) = ("9", "9");

    fn ledger(entries: Vec<Entry>) -> Ledger {
        let mut entries = entries;
        entries.sort_by(|a, b| a.date.cmp(&b.date));
        Ledger { entries, ..Default::default() }
    }

    #[test]
    fn card_places() {
        let card_place = |d: &str| card_place(d, "HKD", Some("HK"));
        assert_eq!(card_place("MATSUZAKAYA NAGOYA JP JPY 780"), Some("JP".into()));
        assert_eq!(card_place("TEA HOUSE SHENZHEN CN CNY 68.00"), Some("CN".into()));
        assert_eq!(card_place("BEAN THERE LIMITED HK HK"), Some("HK".into()));
        // billed in HKD: bought online, says nothing about where the owner was
        assert_eq!(card_place("UBER *TRIP HELP.UBER.COM NL"), None);
        assert_eq!(card_place("FINNAIR 1051234567890 VANTAA FI"), None);
        assert_eq!(card_place("DCC FEE-NON-HK MERCHANT"), None);
    }

    #[test]
    fn finds_a_trip_with_its_bookings() {
        let mut v = home_life(("2026-05-24", "2026-05-29"));
        v.push(card("2026-04-01", "KLOOK HK LTD", "Travel", -900.)); // booked ahead
        v.push(card("2026-05-24", "MATSUZAKAYA NAGOYA JP JPY 780", "Groceries", -38.75));
        v.push(card("2026-05-24", "DCC FEE-NON-HK MERCHANT", "Fees", -0.4));
        v.push(revolut("2026-05-25", "Meitetsu Railway", "Transport", "JPY", -230.));
        v.push(revolut("2026-05-29", "7-Eleven", "Groceries", "JPY", -9.));
        let l = ledger(v);
        let trips = discover(&l);
        assert_eq!(trips.len(), 1, "{trips:?}");
        let t = &trips[0];
        assert_eq!((t.start.as_str(), t.end.as_str(), t.days()), ("2026-05-24", "2026-05-29", 6));
        assert_eq!(t.places, vec!["Japan".to_string()]);
        assert!((t.away_hkd - 277.75).abs() < 1e-9);
        assert!((t.booked_hkd - 900.4).abs() < 1e-9, "klook + the DCC fee: {}", t.booked_hkd);
        assert_eq!(t.by_category[0].0, "Travel");
    }

    #[test]
    fn online_services_and_lone_purchases_are_not_trips() {
        let mut v = home_life(NEVER);
        for m in ["03", "07", "09"] {
            v.push(card(&format!("2026-{m}-14"), "CLOUDAPI LLC NEW YORK US USD 20.00", "Subscriptions-ish", -156.));
            v.push(card(&format!("2026-{m}-15"), "CLOUDAPI LLC NEW YORK US USD 2.00", "Other", -16.));
        }
        v.push(card("2026-04-16", "PIXELSHOP SAN FRANCISCO US USD 12.00", "Other", -93.6));
        assert!(discover(&ledger(v)).is_empty());
    }

    #[test]
    fn eur_cards_used_from_home_are_not_trips() {
        let mut v = home_life(NEVER);
        for d in ["2026-04-11", "2026-04-11", "2026-04-12"] {
            v.push(revolut(d, "DiDi", "Transport", "EUR", -20.));
        }
        v.push(revolut("2026-04-13", "To Alex Example", "Other", "EUR", -400.));
        v.push(revolut("2026-04-13", "N26", "Fees", "EUR", -40.));
        assert!(discover(&ledger(v)).is_empty());
    }

    #[test]
    fn a_coffee_at_home_splits_two_trips() {
        let mut v = home_life(NEVER);
        v.push(revolut("2026-03-03", "Sushi Corner", "Dining", "JPY", -300.));
        v.push(revolut("2026-03-04", "FamilyMart", "Groceries", "JPY", -50.));
        // home on the 6th, then Shenzhen on the 8th: two trips, not one
        v.push(card("2026-03-08", "LOTUS 8 SHENZHEN CN CNY 236.00", "Dining", -255.1));
        v.push(card("2026-03-08", "GREEN HOUSE SHENZHEN CN CNY 270.00", "Dining", -292.));
        let trips = discover(&ledger(v));
        assert_eq!(trips.iter().map(|t| t.label()).collect::<Vec<_>>(), ["Japan", "China"]);
    }

    #[test]
    fn europe_is_home_until_the_move() {
        // 2025: living on a EUR account; 2026: Hong Kong, with a week in Italy
        let mut v = home_life(("2026-06-20", "2026-06-28"));
        for m in 1..=12 {
            for d in [3, 9, 15, 21, 27] {
                v.push(revolut(&format!("2025-{m:02}-{d:02}"), "Conad", "Groceries", "EUR", -80.));
            }
        }
        v.push(revolut("2026-06-20", "Caffè Stella", "Dining", "EUR", -30.));
        v.push(card("2026-06-26", "AUTOLINEE BIANCHI S.R.L. BARI IT EUR 12.50", "Transport", -113.8));
        v.push(revolut("2026-06-28", "SNCF", "Transport", "EUR", -1200.));
        let trips = discover(&ledger(v));
        assert_eq!(trips.len(), 1, "no trips while Europe was home: {trips:?}");
        assert_eq!(trips[0].start, "2026-06-20");
        // the named country wins over the generic euro spend
        assert_eq!(trips[0].places, vec!["Italy".to_string()]);
    }

    #[test]
    fn transport_modes() {
        let m = |d: &str| transport(d);
        for d in ["FINNAIR 1051234567890 VANTAA FI", "SWISS7241234567891 SHEUNG WAN HK", "EasyJet", "Ryanair",
            "AIR FRANCE", "ITA airways", "turkishairlines.com", "AEROITALIA ONLINE", "Transavia",
            "Deutsche Lufthansa AG", "Norwegian Air Shuttle FX Rate £1 = $1.2700"] {
            assert_eq!(m(d), Some(Mode::Flight), "{d}");
        }
        for d in ["SNCF", "SNCF CONNECT", "Trenitalia", "Nankai Electric Railway", "Eurostar", "SBB CFF FFS Zurich HB",
            "Thetrainline.com", "JR EAST"] {
            assert_eq!(m(d), Some(Mode::Train), "{d}");
        }
        for d in ["Flixbus*Berlin", "AUTOLINEE BIANCHI S.R.L. BARI IT EUR 12.50", "National Express", "Megabus"] {
            assert_eq!(m(d), Some(Mode::Bus), "{d}");
        }
        for d in ["UBER *TRIP HELP.UBER.COM NL", "UBR* PENDING.UBER.COM NL", "DiDi", "FREENOW* ABCD12-3", "HK URBAN TAXI"] {
            assert_eq!(m(d), Some(Mode::Ride), "{d}");
        }
        for d in ["Chiba Monorail", "OSAKAMONORAIL OSAKA JP JPY 320", "Île-de-France Mobilités", "NAVIGO ANNUEL",
            "Tokyo Metro", "OVpay", "TFL", "METROPOLITANA DI NAPOLI", "JANE DOE        OCTOPUS01234567890 02OCT"] {
            assert_eq!(m(d), Some(Mode::Metro), "{d}");
        }
        for d in ["CHU KONG PASSENGER 07 HONG KONG HK", "CHU KONG FERRY 30212 HONG KONG HK", "Caronte & Tourist"] {
            assert_eq!(m(d), Some(Mode::Boat), "{d}");
        }
        for d in ["GRAND PLAZA HOTEL KOWLOON HK", "Airbnb", "Parkhotel Graz", "B AND B LA ROSA", "Hotel9Sleep", "Booking.com"] {
            assert_eq!(m(d), Some(Mode::Stay), "{d}");
        }
        for d in ["Trip.com Travel SG", "TRIP COM TRAVEL SG", "Klook HK"] {
            assert_eq!(m(d), Some(Mode::Booking), "{d}");
        }
        for d in ["UBER *EATS HELP.UBER.COM NL", "MATSUZAKAYA NAGOYA JP JPY 780", "Saily", "Jumbo Supermarkt", "Swissquote"] {
            assert_eq!(m(d), None, "{d}");
        }
    }

    #[test]
    fn a_route_with_stops_and_legs() {
        let mut v = home_life(("2026-05-23", "2026-06-03"));
        v.push(card("2026-05-01", "Trip.com Travel SG", "Travel", -2200.)); // booked ahead: a booking
        v.push(card("2026-05-24", "MATSUZAKAYA NAGOYA JP JPY 780", "Groceries", -40.));
        v.push(revolut("2026-05-25", "Meitetsu Railway", "Transport", "JPY", -230.));
        v.push(revolut("2026-05-26", "Sushi Corner", "Dining", "JPY", -300.)); // names nothing: Nagoya by its neighbours
        v.push(card("2026-05-29", "LAWSON OSAKA JP JPY 500", "Groceries", -25.));
        v.push(revolut("2026-05-29", "JR West", "Transport", "JPY", -700.));
        v.push(card("2026-05-30", "DAIMARU OSAKA JP JPY 9000", "Shopping", -450.));
        v.push(revolut("2026-06-01", "Kansai Airport shop", "Other", "JPY", -50.));
        let l = ledger(v);
        let trips = discover(&l);
        assert_eq!(trips.len(), 1, "{trips:?}");
        let r = route(&l, &trips[0]);
        assert_eq!(r.home.name, "Hong Kong");
        let stops: Vec<(&str, &str, &str)> = r.stops.iter().map(|s| (s.place.name.as_str(), s.first.as_str(), s.last.as_str())).collect();
        assert_eq!(stops, [("Nagoya", "2026-05-24", "2026-05-26"), ("Osaka", "2026-05-29", "2026-06-01")]);
        assert!((r.stops[0].hkd - 570.).abs() < 1e-9);
        let legs: Vec<(&str, &str, Option<Mode>, bool)> =
            r.legs.iter().map(|l| (l.from.name.as_str(), l.to.name.as_str(), l.mode, l.inferred)).collect();
        assert_eq!(legs, [
            ("Hong Kong", "Nagoya", Some(Mode::Flight), false), // the airport row backs the flights
            ("Nagoya", "Osaka", Some(Mode::Train), false),
            ("Osaka", "Hong Kong", Some(Mode::Flight), false),
        ]);
        let modes: Vec<(Mode, usize)> = r.by_mode.iter().map(|(m, n, _)| (*m, *n)).collect();
        assert_eq!(modes, [(Mode::Train, 2), (Mode::Booking, 1)]);
        assert_eq!(r.country_days, [("JP".to_string(), 6)]);
    }

    #[test]
    fn short_hops_without_a_ticket_are_inferred() {
        let mut v = home_life(("2026-03-07", "2026-03-09"));
        v.push(card("2026-03-08", "LOTUS 8 SHENZHEN CN CNY 236.00", "Dining", -255.1));
        v.push(card("2026-03-08", "GREEN HOUSE SHENZHEN CN CNY 270.00", "Dining", -292.));
        let l = ledger(v);
        let r = route(&l, &discover(&l)[0]);
        assert_eq!(r.stops.len(), 1);
        assert_eq!(r.stops[0].place.name, "Shenzhen");
        assert!(r.legs.iter().all(|l| l.inferred && l.mode.is_none()), "{:?}", r.legs);
        // with a ferry ticket the crossing is known
        let mut v = home_life(("2026-03-07", "2026-03-09"));
        v.push(card("2026-03-07", "CHU KONG FERRY 30212 HONG KONG HK", "Travel", -570.));
        v.push(card("2026-03-08", "LOTUS 8 SHENZHEN CN CNY 236.00", "Dining", -255.1));
        v.push(card("2026-03-08", "GREEN HOUSE SHENZHEN CN CNY 270.00", "Dining", -292.));
        let l = ledger(v);
        let r = route(&l, &discover(&l)[0]);
        assert!(r.legs.iter().all(|l| !l.inferred && l.mode == Some(Mode::Boat)), "{:?}", r.legs);
    }
}
