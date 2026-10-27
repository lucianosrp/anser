//! Asset classes: what each holding is (equity, a bond and which kind, cash,
//! crypto, a commodity), who issues it, and the mix they add up to.
//!
//! Classification is a small static table by symbol ([`SECURITIES`]) and,
//! for anything not in it, plain-word heuristics on the description
//! ("Treasury", "T-Bill", "Gilt", "Bond", "Gold", ...). A holding neither
//! knows stays [`AssetClass::Unclassified`]; nothing is guessed from the
//! ticker's letters. The issuer comes from the same table, then from the
//! description ("Vanguard", "VANG ", "Invesco", "iShares", "SPDR",
//! "Schwab", "HSBC").
//!
//! Conventions, so the numbers read the same everywhere:
//! * a bond fund's sub-kind is what its index holds: T-bills and money
//!   under a year are [`BondKind::TBill`]; government and gilt funds of
//!   longer maturity are [`BondKind::Government`]; investment-grade and
//!   high-yield company debt is [`BondKind::Corporate`]; anything mixing
//!   them (or a description that only says "bond") is
//!   [`BondKind::Aggregate`]
//! * broker cash, bank cash, money-market funds and dollar stablecoins are
//!   [`AssetClass::Cash`]
//! * the bond/stock ratio is bonds against equity only: bonds ÷ (equity +
//!   bonds); cash, crypto and commodities sit outside it

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AssetClass {
    Equity,
    Bond,
    Cash,
    Crypto,
    Commodity,
    /// nothing in the table or the description says what it is
    Unclassified,
}

impl AssetClass {
    /// Display order: the mix bar and table list classes in this order.
    pub const ALL: [AssetClass; 6] =
        [AssetClass::Equity, AssetClass::Bond, AssetClass::Cash, AssetClass::Crypto, AssetClass::Commodity, AssetClass::Unclassified];

    pub fn label(self) -> &'static str {
        match self {
            AssetClass::Equity => "Equity",
            AssetClass::Bond => "Bonds",
            AssetClass::Cash => "Cash",
            AssetClass::Crypto => "Crypto",
            AssetClass::Commodity => "Commodities",
            AssetClass::Unclassified => "Not classified",
        }
    }

    /// the key in a target-mix file (`[mix] equity = 80`)
    pub fn key(self) -> &'static str {
        match self {
            AssetClass::Equity => "equity",
            AssetClass::Bond => "bond",
            AssetClass::Cash => "cash",
            AssetClass::Crypto => "crypto",
            AssetClass::Commodity => "commodity",
            AssetClass::Unclassified => "unclassified",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BondKind {
    /// Treasury bills and other debt maturing within about a year
    TBill,
    /// government bonds (Treasuries, gilts) beyond a year
    Government,
    /// company debt
    Corporate,
    /// a mix of government and corporate (an "aggregate" index), or unspecified
    Aggregate,
}

impl BondKind {
    pub const ALL: [BondKind; 4] = [BondKind::TBill, BondKind::Government, BondKind::Corporate, BondKind::Aggregate];

    pub fn label(self) -> &'static str {
        match self {
            BondKind::TBill => "T-bills, short-term",
            BondKind::Government => "Government",
            BondKind::Corporate => "Corporate",
            BondKind::Aggregate => "Aggregate",
        }
    }
}

/// How a holding is held: drives the icon when there's no brand logo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Vehicle {
    Etf,
    Stock,
    /// a mutual fund (HSBC FlexInvest)
    Fund,
    Coin,
    Cash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Issuer {
    Vanguard,
    Invesco,
    /// BlackRock's ETF brand
    IShares,
    /// State Street's ETF brand
    Spdr,
    Schwab,
    Hsbc,
}

impl Issuer {
    pub fn label(self) -> &'static str {
        match self {
            Issuer::Vanguard => "Vanguard",
            Issuer::Invesco => "Invesco",
            Issuer::IShares => "iShares",
            Issuer::Spdr => "SPDR",
            Issuer::Schwab => "Schwab",
            Issuer::Hsbc => "HSBC",
        }
    }
}

/// What a holding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Classification {
    pub class: AssetClass,
    /// set when `class` is [`AssetClass::Bond`]
    pub bond: Option<BondKind>,
    pub vehicle: Option<Vehicle>,
    pub issuer: Option<Issuer>,
}

impl Classification {
    pub const UNKNOWN: Classification = Classification { class: AssetClass::Unclassified, bond: None, vehicle: None, issuer: None };
    pub const CASH: Classification = Classification { class: AssetClass::Cash, bond: None, vehicle: Some(Vehicle::Cash), issuer: None };
    pub const COIN: Classification = Classification { class: AssetClass::Crypto, bond: None, vehicle: Some(Vehicle::Coin), issuer: None };

    /// "Equity ETF", "T-bill ETF", "Stock", "Coin"
    pub fn describe(&self) -> String {
        let what = match (self.class, self.bond) {
            (AssetClass::Bond, Some(BondKind::TBill)) => "T-bill",
            (AssetClass::Bond, Some(BondKind::Government)) => "Government bond",
            (AssetClass::Bond, Some(BondKind::Corporate)) => "Corporate bond",
            (AssetClass::Bond, _) => "Bond",
            (AssetClass::Equity, _) => "Equity",
            (AssetClass::Cash, _) => "Cash",
            (AssetClass::Crypto, _) => "Crypto",
            (AssetClass::Commodity, _) => "Commodity",
            (AssetClass::Unclassified, _) => "Not classified",
        };
        match self.vehicle {
            Some(Vehicle::Etf) => format!("{what} ETF"),
            Some(Vehicle::Fund) => format!("{what} fund"),
            Some(Vehicle::Stock) => "Stock".into(),
            Some(Vehicle::Coin) if self.class == AssetClass::Cash => "Stablecoin".into(),
            Some(Vehicle::Coin) => "Coin".into(),
            _ => what.into(),
        }
    }
}

/// One row of the static table.
pub struct Security {
    pub symbol: &'static str,
    pub class: AssetClass,
    pub bond: Option<BondKind>,
    pub vehicle: Vehicle,
    pub issuer: Option<Issuer>,
}

const fn s(symbol: &'static str, class: AssetClass, bond: Option<BondKind>, vehicle: Vehicle, issuer: Option<Issuer>) -> Security {
    Security { symbol, class, bond, vehicle, issuer }
}

use AssetClass::{Bond, Commodity, Equity};
use BondKind::{Aggregate, Corporate, Government, TBill};
use Issuer::{Hsbc, IShares, Invesco, Schwab, Spdr, Vanguard};
use Vehicle::{Etf, Fund, Stock};

/// Common funds (facts about the fund: its index), not recommendations.
/// Single stocks are known by [`super::COMPANIES`] or their description.
pub const SECURITIES: &[Security] = &[
    // ---- Vanguard
    s("VWRA", Equity, None, Etf, Some(Vanguard)), // FTSE All-World, USD acc (LSE)
    s("VWRL", Equity, None, Etf, Some(Vanguard)), // FTSE All-World, dist
    s("VT", Equity, None, Etf, Some(Vanguard)),   // Total World Stock
    s("VTI", Equity, None, Etf, Some(Vanguard)),  // Total (US) Stock Market
    s("VOO", Equity, None, Etf, Some(Vanguard)),  // S&P 500
    s("VUAA", Equity, None, Etf, Some(Vanguard)), // S&P 500 UCITS acc
    s("VUSA", Equity, None, Etf, Some(Vanguard)), // S&P 500 UCITS dist
    s("VEA", Equity, None, Etf, Some(Vanguard)),  // developed ex-US
    s("VWO", Equity, None, Etf, Some(Vanguard)),  // emerging markets
    s("VXUS", Equity, None, Etf, Some(Vanguard)), // total international
    s("BND", Bond, Some(Aggregate), Etf, Some(Vanguard)),  // US total bond market
    s("BNDX", Bond, Some(Aggregate), Etf, Some(Vanguard)), // international bonds, hedged
    s("VAGU", Bond, Some(Aggregate), Etf, Some(Vanguard)), // global aggregate UCITS
    s("VGSH", Bond, Some(Government), Etf, Some(Vanguard)), // 1-3y Treasuries
    s("VGIT", Bond, Some(Government), Etf, Some(Vanguard)), // 3-10y Treasuries
    s("VGOV", Bond, Some(Government), Etf, Some(Vanguard)), // UK gilts
    s("VCIT", Bond, Some(Corporate), Etf, Some(Vanguard)),  // intermediate corporates
    // ---- Invesco
    s("QQQ", Equity, None, Etf, Some(Invesco)),  // Nasdaq-100
    s("QQQM", Equity, None, Etf, Some(Invesco)), // Nasdaq-100
    s("EQQQ", Equity, None, Etf, Some(Invesco)), // Nasdaq-100 UCITS (LSE)
    s("EQAC", Equity, None, Etf, Some(Invesco)), // Nasdaq-100 UCITS acc
    s("RSP", Equity, None, Etf, Some(Invesco)),  // S&P 500 equal weight
    // ---- iShares (BlackRock)
    s("IB01", Bond, Some(TBill), Etf, Some(IShares)), // $ Treasury Bond 0-1yr UCITS
    s("SGOV", Bond, Some(TBill), Etf, Some(IShares)), // 0-3 month Treasury
    s("SHV", Bond, Some(TBill), Etf, Some(IShares)),  // short Treasury (under a year)
    s("IBTA", Bond, Some(Government), Etf, Some(IShares)), // $ Treasury 1-3yr UCITS
    s("IDTL", Bond, Some(Government), Etf, Some(IShares)), // $ Treasury 20+yr UCITS
    s("TLT", Bond, Some(Government), Etf, Some(IShares)),  // 20+ year Treasury
    s("IEF", Bond, Some(Government), Etf, Some(IShares)),  // 7-10 year Treasury
    s("IGLT", Bond, Some(Government), Etf, Some(IShares)), // UK gilts
    s("AGG", Bond, Some(Aggregate), Etf, Some(IShares)),   // US aggregate
    s("AGGU", Bond, Some(Aggregate), Etf, Some(IShares)),  // global aggregate UCITS, USD hedged
    s("AGGG", Bond, Some(Aggregate), Etf, Some(IShares)),  // global aggregate UCITS
    s("LQD", Bond, Some(Corporate), Etf, Some(IShares)),   // $ investment-grade corporates
    s("LQDE", Bond, Some(Corporate), Etf, Some(IShares)),  // $ corporates UCITS
    s("CSPX", Equity, None, Etf, Some(IShares)), // Core S&P 500 UCITS
    s("IWDA", Equity, None, Etf, Some(IShares)), // Core MSCI World UCITS
    s("SWDA", Equity, None, Etf, Some(IShares)), // Core MSCI World UCITS (GBP line)
    s("URTH", Equity, None, Etf, Some(IShares)), // MSCI World
    s("ISAC", Equity, None, Etf, Some(IShares)), // MSCI ACWI UCITS
    s("SSAC", Equity, None, Etf, Some(IShares)), // MSCI ACWI UCITS
    s("EIMI", Equity, None, Etf, Some(IShares)), // Core MSCI EM IMI UCITS
    s("IVV", Equity, None, Etf, Some(IShares)),  // Core S&P 500
    s("SGLN", Commodity, None, Etf, Some(IShares)), // physical gold
    s("IAU", Commodity, None, Etf, Some(IShares)),  // gold trust
    // ---- SPDR (State Street)
    s("SPY", Equity, None, Etf, Some(Spdr)),
    s("SPLG", Equity, None, Etf, Some(Spdr)),
    s("BIL", Bond, Some(TBill), Etf, Some(Spdr)), // 1-3 month T-bill
    s("GLD", Commodity, None, Etf, Some(Spdr)),   // gold shares
    // ---- Schwab
    s("SCHB", Equity, None, Etf, Some(Schwab)),
    s("SCHD", Equity, None, Etf, Some(Schwab)),
    s("SCHX", Equity, None, Etf, Some(Schwab)),
    s("SCHO", Bond, Some(Government), Etf, Some(Schwab)), // short-term (1-3y) Treasuries
    s("SCHZ", Bond, Some(Aggregate), Etf, Some(Schwab)),
    // ---- HSBC: the MSCI World index fund the hsbc_sms source's
    // confirmations name
    s("U50004", Equity, None, Fund, Some(Hsbc)),
];

pub fn security(symbol: &str) -> Option<&'static Security> {
    SECURITIES.iter().find(|x| x.symbol.eq_ignore_ascii_case(symbol))
}

/// Upper-case words, punctuation as spaces, padded: " ISHARES $ TREASURY ".
fn words(text: &str) -> String {
    let cleaned: String = text.to_uppercase().chars().map(|c| if c.is_alphanumeric() || c == '&' { c } else { ' ' }).collect();
    format!(" {} ", cleaned.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn has(w: &str, keys: &[&str]) -> bool {
    keys.iter().any(|k| w.contains(&format!(" {k} ")))
}

/// The issuer named in a description ("VANG FTSE AW USDA" is Vanguard).
pub fn issuer_in(description: &str) -> Option<Issuer> {
    let w = words(description);
    [
        (&["VANGUARD", "VANG"][..], Vanguard),
        (&["INVESCO"][..], Invesco),
        (&["ISHARES", "ISH", "BLACKROCK"][..], IShares),
        (&["SPDR"][..], Spdr),
        (&["SCHWAB"][..], Schwab),
        (&["HSBC"][..], Hsbc),
    ]
    .into_iter()
    .find(|(k, _)| has(&w, k))
    .map(|(_, i)| i)
}

/// Asset class from a description's words, when they say.
fn class_in(description: &str) -> Option<(AssetClass, Option<BondKind>)> {
    let w = words(description);
    let w = w.replace(" T BILL", " TBILL").replace(" TREASURY BILL", " TBILL");
    if has(&w, &["TBILL", "TBILLS", "0 1YR", "0 1 YR", "0 3 MONTH", "0 3M", "1 3 MONTH", "ULTRASHORT"]) {
        return Some((Bond, Some(TBill)));
    }
    if has(&w, &["MONEY MARKET", "MMF", "LIQUIDITY FUND", "CASH FUND"]) {
        return Some((AssetClass::Cash, None));
    }
    if has(&w, &["GOLD", "SILVER", "COMMODITY", "COMMODITIES", "PRECIOUS METALS"]) {
        return Some((Commodity, None));
    }
    let bondish = has(&w, &["BOND", "BONDS", "BD", "FIXED INCOME", "TREASURY", "TREASURIES", "TREAS", "GILT", "GILTS", "GOVT", "GOVERNMENT", "AGGREGATE", "AGG", "CORP", "CORPORATE"]);
    if bondish && !has(&w, &["EQUITY", "STOCK", "STK"]) {
        let kind = if has(&w, &["AGGREGATE", "AGG"]) {
            Aggregate
        } else if has(&w, &["CORP", "CORPORATE", "INVESTMENT GRADE", "HIGH YIELD", "CREDIT"]) && has(&w, &["BOND", "BONDS", "BD", "FIXED INCOME"]) {
            Corporate
        } else if has(&w, &["TREASURY", "TREASURIES", "TREAS", "GILT", "GILTS", "GOVT", "GOVERNMENT"]) {
            Government
        } else if has(&w, &["BOND", "BONDS", "BD", "FIXED INCOME"]) {
            Aggregate
        } else {
            // "CORP" alone is a company name ("MICROSOFT CORP"), not a bond
            return None;
        };
        return Some((Bond, Some(kind)));
    }
    if has(&w, &["EQUITY", "EQUITIES", "STOCK", "STK", "SHARES", "INDEX", "NASDAQ", "NASDAQ 100", "S&P", "S&P 500", "MSCI", "FTSE", "WORLD", "DOW", "RUSSELL", "TECHNOLOGY"]) {
        return Some((Equity, None));
    }
    None
}

/// How it's held, from a description: an ETF/trust, or a company's shares.
fn vehicle_in(description: &str) -> Option<Vehicle> {
    let w = words(description);
    if has(&w, &["ETF", "UCITS", "TRUST", "ETC", "INDEX FUND"]) {
        Some(Etf)
    } else if has(&w, &["FUND"]) {
        Some(Fund)
    } else if has(&w, &["CORP", "INC", "CO", "LTD", "PLC", "SA", "AG", "NV", "HOLDINGS", "GROUP", "CL A", "CL B", "CLASS A", "CLASS B"]) {
        Some(Stock)
    } else {
        None
    }
}

/// A security: the table first, then the description's words. Coins and
/// cash are known by their account, not here (see `portfolio::Holding`).
pub fn classify(symbol: &str, description: &str) -> Classification {
    if let Some(x) = security(symbol) {
        return Classification { class: x.class, bond: x.bond, vehicle: Some(x.vehicle), issuer: x.issuer };
    }
    if super::company(symbol).is_some() {
        return Classification { class: Equity, bond: None, vehicle: Some(Stock), issuer: None };
    }
    let issuer = issuer_in(description);
    let vehicle = vehicle_in(description).or(issuer.map(|_| Etf));
    let (class, bond) = match class_in(description) {
        Some(c) => c,
        // a company's own shares are equity
        None if vehicle == Some(Stock) => (Equity, None),
        None => (AssetClass::Unclassified, None),
    };
    Classification { class, bond, vehicle, issuer }
}

/// A coin: dollar stablecoins count as cash, the rest as crypto.
pub fn classify_coin(symbol: &str) -> Classification {
    if crate::portfolio::STABLE.iter().any(|s| s.eq_ignore_ascii_case(symbol)) {
        Classification { class: AssetClass::Cash, ..Classification::COIN }
    } else {
        Classification::COIN
    }
}

// ------------------------------------------------------------------- mix

/// The mix: HKD per class (in [`AssetClass::ALL`] order) and per bond kind.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mix {
    pub total: f64,
    pub by_class: [f64; 6],
    pub by_bond: [f64; 4],
}

impl Mix {
    pub fn add(&mut self, c: &Classification, hkd: f64) {
        self.total += hkd;
        self.by_class[c.class.index()] += hkd;
        if c.class == AssetClass::Bond {
            self.by_bond[c.bond.unwrap_or(Aggregate) as usize] += hkd;
        }
    }

    pub fn of(items: &[(Classification, f64)]) -> Mix {
        let mut m = Mix::default();
        for (c, v) in items {
            m.add(c, *v);
        }
        m
    }

    pub fn hkd(&self, c: AssetClass) -> f64 {
        self.by_class[c.index()]
    }

    pub fn pct(&self, c: AssetClass) -> f64 {
        if self.total > 0. { self.hkd(c) / self.total * 100. } else { 0. }
    }

    /// The bond/stock ratio as (equity %, bond %) of equity + bonds; None
    /// when neither is held. No bonds reads (100, 0).
    pub fn stock_bond(&self) -> Option<(f64, f64)> {
        let (e, b) = (self.hkd(AssetClass::Equity), self.hkd(AssetClass::Bond));
        (e + b > 0.).then(|| (e / (e + b) * 100., b / (e + b) * 100.))
    }

    /// The mix with `extra` HKD more of a class (bank cash, when included).
    pub fn plus(&self, c: AssetClass, extra: f64) -> Mix {
        let mut m = self.clone();
        m.add(&Classification { class: c, ..Classification::UNKNOWN }, extra);
        m
    }
}

/// One class against a target: (class, now %, target %, now − target in
/// points, HKD to move to reach the target at today's total; + = above).
pub type Drift = (AssetClass, f64, f64, f64, f64);

/// The mix against a target (`class key -> weight`, normalized to 100%;
/// unknown keys ignored). Classes missing from the target count as 0%.
/// Empty when the target has no positive weights.
pub fn drift(mix: &Mix, target: &BTreeMap<String, f64>) -> Vec<Drift> {
    let w = |c: AssetClass| target.get(c.key()).copied().filter(|x| x.is_finite() && *x > 0.).unwrap_or(0.);
    let sum: f64 = AssetClass::ALL.iter().map(|c| w(*c)).sum();
    if sum <= 0. {
        return Vec::new();
    }
    AssetClass::ALL.iter()
        .filter(|c| w(**c) > 0. || mix.hkd(**c).abs() > 1e-9)
        .map(|c| {
            let (now, goal) = (mix.pct(*c), w(*c) / sum * 100.);
            (*c, now, goal, now - goal, mix.hkd(*c) - goal / 100. * mix.total)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn funds_and_stocks() {
        for (sym, issuer) in [("VWRA", Vanguard), ("VT", Vanguard), ("QQQ", Invesco), ("EQQQ", Invesco)] {
            let c = classify(sym, "");
            assert_eq!((c.class, c.issuer, c.vehicle), (Equity, Some(issuer), Some(Etf)), "{sym}");
        }
        for (sym, desc) in [("AAPL", ""), ("ACME", "ACME CORP"), ("ACME", "ACME SPACE TECHN-CL A")] {
            let n = classify(sym, desc);
            assert_eq!((n.class, n.vehicle, n.issuer), (Equity, Some(Stock), None), "{sym} {desc}");
        }
        assert_eq!(classify("U50004", "HSBC FlexInvest").issuer, Some(Hsbc));
    }

    #[test]
    fn bonds_and_bills() {
        let k = |sym: &str| { let c = classify(sym, ""); (c.class, c.bond) };
        assert_eq!(k("IB01"), (Bond, Some(TBill)));
        assert_eq!(k("BIL"), (Bond, Some(TBill)));
        assert_eq!(k("SGOV"), (Bond, Some(TBill)));
        assert_eq!(k("AGGU"), (Bond, Some(Aggregate)));
        assert_eq!(k("AGG"), (Bond, Some(Aggregate)));
        assert_eq!(k("IGLT"), (Bond, Some(Government)));
        assert_eq!(k("LQD"), (Bond, Some(Corporate)));
        assert_eq!(k("GLD"), (Commodity, None));
        assert_eq!(classify("IB01", "").issuer, Some(IShares));
        assert_eq!(classify("SPY", "").issuer, Some(Spdr));
        for sym in ["CSPX", "IWDA", "URTH"] {
            assert_eq!(classify(sym, "").issuer, Some(IShares), "{sym}");
        }
    }

    #[test]
    fn descriptions_when_the_table_is_silent() {
        let c = classify("XYZ1", "ISHARES $ TREASURY BD 0-1YR");
        assert_eq!((c.class, c.bond, c.issuer, c.vehicle), (Bond, Some(TBill), Some(IShares), Some(Etf)));
        let c = classify("XYZ2", "SPDR BLOOMBERG 1-3 MONTH T-BILL ETF");
        assert_eq!((c.class, c.bond, c.issuer), (Bond, Some(TBill), Some(Spdr)));
        let c = classify("XYZ3", "VANGUARD UK GILT UCITS ETF");
        assert_eq!((c.class, c.bond, c.issuer), (Bond, Some(Government), Some(Vanguard)));
        let c = classify("XYZ4", "ISHARES CORE GLOBAL AGGREGATE BOND");
        assert_eq!(c.bond, Some(Aggregate));
        let c = classify("XYZ5", "ISHARES $ CORP BOND UCITS");
        assert_eq!(c.bond, Some(Corporate));
        let c = classify("XYZ6", "INVESCO NASDAQ-100 DIST");
        assert_eq!((c.class, c.issuer), (Equity, Some(Invesco)));
        let c = classify("XYZ7", "VANG FTSE AW USDA");
        assert_eq!((c.class, c.issuer), (Equity, Some(Vanguard)));
        // a company called "... CORP" is a stock, not a corporate bond
        let c = classify("ABCD", "ACME CORP");
        assert_eq!((c.class, c.bond, c.vehicle), (Equity, None, Some(Stock)));
        assert_eq!(classify("ZZZ", "").class, AssetClass::Unclassified);
        assert_eq!(classify("XYZ8", "HSBC Global Bond Index Fund").class, Bond);
        assert_eq!(classify("XYZ9", "HSBC Global Money Market Fund").class, AssetClass::Cash);
        assert_eq!(classify("XYZ0", "iShares Physical Gold ETC").class, Commodity);
    }

    #[test]
    fn coins() {
        assert_eq!(classify_coin("BTC").class, AssetClass::Crypto);
        assert_eq!(classify_coin("USDT").class, AssetClass::Cash);
    }

    #[test]
    fn mix_ratio_and_drift() {
        let eq = classify("VWRA", "");
        let m = Mix::of(&[(eq, 800.), (Classification::CASH, 200.)]);
        assert_eq!(m.stock_bond(), Some((100., 0.)));
        let m = m.plus(AssetClass::Bond, 200.);
        assert_eq!(m.by_bond[Aggregate as usize], 200.);
        let (e, b) = m.stock_bond().unwrap();
        assert!((e - 80.).abs() < 1e-9 && (b - 20.).abs() < 1e-9);
        let m = Mix::of(&[(eq, 600.), (classify("IB01", ""), 200.), (Classification::CASH, 200.)]);
        assert_eq!(m.by_bond[TBill as usize], 200.);
        let target: BTreeMap<String, f64> = [("equity".to_string(), 70.), ("bond".to_string(), 30.)].into();
        let d = drift(&m, &target);
        let eqd = d.iter().find(|x| x.0 == Equity).unwrap();
        assert!((eqd.1 - 60.).abs() < 1e-9 && (eqd.2 - 70.).abs() < 1e-9 && (eqd.3 + 10.).abs() < 1e-9 && (eqd.4 + 100.).abs() < 1e-9);
        let cash = d.iter().find(|x| x.0 == AssetClass::Cash).unwrap();
        assert!((cash.2).abs() < 1e-9 && (cash.4 - 200.).abs() < 1e-9);
        assert!(drift(&m, &BTreeMap::new()).is_empty());
        assert_eq!(Mix::default().stock_bond(), None);
    }
}
