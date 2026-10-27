//! Rule-based transaction categorization.
//!
//! Rules live in the editable `category_rules` table (seeded once from
//! DEFAULT_RULES). Each rule is a lowercase substring matched against a
//! transaction's description; the lowest-priority-number match wins (ties:
//! lowest id). A pattern starting with "=" must equal the whole (trimmed,
//! lowercased) description -- for N26 pocket moves, whose description is
//! just the pocket's name ("House", "Salary") and would otherwise also hit
//! "BAKEHOUSE" or a payroll line. direction 'in' / 'out' limits a rule to
//! inflows or outflows (a landlord's deposit refund vs its rent debits).
//! share is the fraction of each matching row that is the owner's own money
//! (a shared rent counts only the owner's part as spend). One-off exceptions
//! go in txn_overrides (set_override), which beat every rule. Rules flagged
//! is_transfer=1 mark money moving between own accounts/exchanges -- excluded
//! from spending totals so an internal move never looks like an expense.
//!
//! Categories are stored on bank_transactions.category by categorize_all(),
//! which runs automatically after each ingest. Re-running is idempotent.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::sources::textfmt;

/// (priority, pattern, category, is_transfer, direction, share): the generic
/// seed a fresh install starts from. The order is the historical one (ids
/// decide ties within a priority), minus one person's rules (an employer, family, landlords and
/// a rent share, their own names, a partner, an office): those live in that
/// person's database as user rules (Phase 3 made every existing rule one),
/// and this list only ever seeds an empty rule table.
pub const DEFAULT_RULES: &[(i64, &str, &str, i64, Option<&str>, Option<f64>)] = &[
    (10, "salary deposit", "Salary", 0, None, None),
    (29, "=salary", "Internal transfer", 1, None, None),
    (41, "credit interest", "Interest / rebates", 0, None, None),
    (41, "cash rebate", "Interest / rebates", 0, None, None),
    (41, "saving rate", "Interest / rebates", 0, None, None),
    (20, "apartment rent", "Rent", 0, None, None),
    (25, "interactive brokers", "Investments", 1, None, None),
    (25, "sec     p", "Investments", 1, None, None),
    (25, "to investment", "Investments", 1, None, None),
    (24, "octopus0", "Transport", 0, None, None),
    (26, "money added via", "Transfer: self/exchange", 1, None, None),
    (26, "auto top-up from", "Transfer: self/exchange", 1, None, None),
    (26, "paysafe", "Transfer: self/exchange", 1, None, None),
    (27, "bought btc with", "Transfer: self/exchange", 1, None, None),
    (27, "bought gbp with", "FX exchange", 1, None, None),
    (27, "exchanged to", "FX exchange", 1, None, None),
    (27, "gold/exchange", "FX exchange", 1, None, None),
    (23, "loans company", "Student loan", 0, None, None),
    (23, "slc ", "Student loan", 0, None, None),
    (30, "main account", "Internal transfer", 1, None, None),
    (30, "instant savings", "Internal transfer", 1, None, None),
    (30, "savings pocket", "Internal transfer", 1, None, None),
    (30, "=house", "Internal transfer", 1, None, None),
    (30, "=subscriptions", "Internal transfer", 1, None, None),
    (31, "to eur", "Internal transfer", 1, None, None),
    (31, "to gbp", "Internal transfer", 1, None, None),
    (31, "to hkd", "Internal transfer", 1, None, None),
    (35, "credit card payment", "Credit-card bill", 1, None, None),
    (34, "=payment - thank you", "Credit-card bill", 1, None, None),
    (24, "dcc fee", "Fees", 0, None, None),
    (40, "atm withdrawal", "Cash / ATM", 0, None, None),
    (40, "cash withdrawal", "Cash / ATM", 0, None, None),
    (40, "to payme", "Cash / ATM", 0, None, None),
    (24, "flexinvest fee", "Fees", 0, None, None),
    (66, "n26", "Fees", 0, None, None),
    (50, "wellcome", "Groceries", 0, None, None),
    (50, "parknshop", "Groceries", 0, None, None),
    (50, "park'n", "Groceries", 0, None, None),
    (50, "city super", "Groceries", 0, None, None),
    (50, "citysuper", "Groceries", 0, None, None),
    (50, "market place", "Groceries", 0, None, None),
    (50, "supermarket", "Groceries", 0, None, None),
    (50, "aeon", "Groceries", 0, None, None),
    (50, "sainsbury", "Groceries", 0, None, None),
    (50, "tesco", "Groceries", 0, None, None),
    (50, "publix", "Groceries", 0, None, None),
    (50, "walmart", "Groceries", 0, None, None),
    (50, "grocery", "Groceries", 0, None, None),
    (50, "lidl", "Groceries", 0, None, None),
    (50, "cooperative food", "Groceries", 0, None, None),
    (50, "coop", "Groceries", 0, None, None),
    (50, "rewe", "Groceries", 0, None, None),
    (50, "conad", "Groceries", 0, None, None),
    (50, "jumbo", "Groceries", 0, None, None),
    (50, "salumeria", "Groceries", 0, None, None),
    (50, "marks & spencer", "Groceries", 0, None, None),
    (50, "marksspencer", "Groceries", 0, None, None),
    (50, "familymart", "Groceries", 0, None, None),
    (50, "7-eleven", "Groceries", 0, None, None),
    (50, "supermar", "Groceries", 0, None, None),
    (50, "donki", "Groceries", 0, None, None),
    (55, "octopus", "Transport", 0, None, None),
    (55, "railway", "Transport", 0, None, None),
    (55, "mtr ", "Transport", 0, None, None),
    (55, "keisei", "Transport", 0, None, None),
    (55, "uber", "Transport", 0, None, None),
    (55, "taxi", "Transport", 0, None, None),
    (55, "metro", "Transport", 0, None, None),
    (54, "uber eats", "Dining", 0, None, None),
    (54, "uber *eats", "Dining", 0, None, None),
    (55, "trainline", "Transport", 0, None, None),
    (55, "tfl", "Transport", 0, None, None),
    (55, "navigo", "Transport", 0, None, None),
    (55, "ovpay", "Transport", 0, None, None),
    (55, "mobilit", "Transport", 0, None, None),
    (55, "freenow", "Transport", 0, None, None),
    (55, "didi", "Transport", 0, None, None),
    (55, "sbb ", "Transport", 0, None, None),
    (60, "air france", "Travel", 0, None, None),
    (60, "ita airways", "Travel", 0, None, None),
    (60, "turkish", "Travel", 0, None, None),
    (60, "finnair", "Travel", 0, None, None),
    (60, "swiss", "Travel", 0, None, None),
    (60, "trip.com", "Travel", 0, None, None),
    (60, "airbnb", "Travel", 0, None, None),
    (60, "booking.com", "Travel", 0, None, None),
    (60, "hotel", "Travel", 0, None, None),
    (60, "ryanair", "Travel", 0, None, None),
    (60, "lufthansa", "Travel", 0, None, None),
    (60, "eurostar", "Travel", 0, None, None),
    (60, "flixbus", "Travel", 0, None, None),
    (60, "aeroitalia", "Travel", 0, None, None),
    (60, "b and b", "Travel", 0, None, None),
    (60, "easyjet", "Travel", 0, None, None),
    (60, "transavia", "Travel", 0, None, None),
    (60, "klook", "Travel", 0, None, None),
    (60, "saily", "Travel", 0, None, None),
    (60, "norwegian", "Travel", 0, None, None),
    (60, "trip com", "Travel", 0, None, None),
    (55, "sncf", "Transport", 0, None, None),
    (55, "trenitalia", "Transport", 0, None, None),
    (65, "spotify", "Subscriptions", 0, None, None),
    (65, "openai", "Subscriptions", 0, None, None),
    (65, "google", "Subscriptions", 0, None, None),
    (65, "netflix", "Subscriptions", 0, None, None),
    (65, "icloud", "Subscriptions", 0, None, None),
    (65, "apple.com", "Subscriptions", 0, None, None),
    (65, "amazon prime", "Subscriptions", 0, None, None),
    (65, "anthropic", "Subscriptions", 0, None, None),
    (65, "claude", "Subscriptions", 0, None, None),
    (65, "nordvpn", "Subscriptions", 0, None, None),
    (65, "oculus", "Subscriptions", 0, None, None),
    (65, "openrouter", "Subscriptions", 0, None, None),
    (70, "telecom italia", "Utilities / Telecom", 0, None, None),
    (70, "vodafone", "Utilities / Telecom", 0, None, None),
    (70, "three", "Utilities / Telecom", 0, None, None),
    (70, "electricity", "Utilities / Telecom", 0, None, None),
    (70, "voxi", "Utilities / Telecom", 0, None, None),
    (70, "windtre", "Utilities / Telecom", 0, None, None),
    (75, "dji", "Shopping", 0, None, None),
    (75, "samsonite", "Shopping", 0, None, None),
    (75, "argos", "Shopping", 0, None, None),
    (75, "amazon", "Shopping", 0, None, None),
    (75, "uniqlo", "Shopping", 0, None, None),
    (75, "debenhams", "Shopping", 0, None, None),
    (75, "loft", "Shopping", 0, None, None),
    (75, "wilko", "Shopping", 0, None, None),
    (75, "fortress", "Shopping", 0, None, None),
    (75, "decathlon", "Shopping", 0, None, None),
    (75, "skechers", "Shopping", 0, None, None),
    (75, "momax", "Shopping", 0, None, None),
    (75, "americaneagle", "Shopping", 0, None, None),
    (75, "ikea", "Shopping", 0, None, None),
    (75, "muji", "Shopping", 0, None, None),
    (76, "farmacia", "Health", 0, None, None),
    (76, "pharmacie", "Health", 0, None, None),
    (76, "ortopedico", "Health", 0, None, None),
    (76, "dr. ", "Health", 0, None, None),
    (76, "watson", "Health", 0, None, None),
    (76, "clinic", "Health", 0, None, None),
    (76, "dental", "Health", 0, None, None),
    (77, "udemy", "Education", 0, None, None),
    (78, "cinema", "Entertainment", 0, None, None),
    (80, "restaurant", "Dining", 0, None, None),
    (80, "cafe", "Dining", 0, None, None),
    (80, "coffee", "Dining", 0, None, None),
    (80, "starbucks", "Dining", 0, None, None),
    (80, "mcdonald", "Dining", 0, None, None),
    (80, "deliveroo", "Dining", 0, None, None),
    (80, "foodpanda", "Dining", 0, None, None),
    (80, "pizzeria", "Dining", 0, None, None),
    (80, "sushi", "Dining", 0, None, None),
    (80, "osteria", "Dining", 0, None, None),
    (80, "ramen", "Dining", 0, None, None),
    (80, "bistrot", "Dining", 0, None, None),
    (80, "nandos", "Dining", 0, None, None),
    (80, "wagamama", "Dining", 0, None, None),
    (80, "bar ", "Dining", 0, None, None),
    (80, "pasticceria", "Dining", 0, None, None),
    (80, "autogrill", "Dining", 0, None, None),
    (80, "chipotle", "Dining", 0, None, None),
    (80, "burger", "Dining", 0, None, None),
    (80, "domino", "Dining", 0, None, None),
    (80, "bento", "Dining", 0, None, None),
    (80, "gyukaku", "Dining", 0, None, None),
    (80, "steak", "Dining", 0, None, None),
    (80, "crystal jade", "Dining", 0, None, None),
    (80, "pizza", "Dining", 0, None, None),
    (80, "pret a manger", "Dining", 0, None, None),
    (80, "ootoya", "Dining", 0, None, None),
    (80, "arabica", "Dining", 0, None, None),
    (80, "waffle", "Dining", 0, None, None),
    (80, "frites", "Dining", 0, None, None),
    (80, "dim sum", "Dining", 0, None, None),
    (80, "noodle", "Dining", 0, None, None),
    (27, "bought doge with", "Transfer: self/exchange", 1, None, None),
    (27, "bought xrp with", "Transfer: self/exchange", 1, None, None),
    (39, "cash at ", "Cash / ATM", 0, None, None),
    (40, "cash deposit", "Cash / ATM", 0, None, None),
    (40, "from payme", "Cash / ATM", 0, None, None),
    (41, "=", "Interest / rebates", 0, Some("in"), None),
    (42, "=", "Taxes", 0, Some("out"), None),
    (41, "=cashback", "Interest / rebates", 0, None, None),
    (66, "immigration departme", "Fees", 0, None, None),
    (65, "paypal europe", "Subscriptions", 0, None, None),
    (65, "paypal (europe)", "Subscriptions", 0, None, None),
    (65, "itunes", "Subscriptions", 0, None, None),
    (65, "namecheap", "Subscriptions", 0, None, None),
    (65, "name-cheap.com", "Subscriptions", 0, None, None),
    (78, "museum", "Entertainment", 0, None, None),
    (78, "steamgames.com", "Entertainment", 0, None, None),
    (78, "youtube super", "Entertainment", 0, None, None),
    (55, "trasporti", "Transport", 0, None, None),
    (55, "trenord", "Transport", 0, None, None),
    (55, "autolinee", "Transport", 0, None, None),
    (55, "monorail", "Transport", 0, None, None),
    (55, "mtr-", "Transport", 0, None, None),
    (55, "tramway", "Transport", 0, None, None),
    (55, "railcard.co.uk", "Transport", 0, None, None),
    (55, "=tsgn", "Transport", 0, None, None),
    (55, "carburanti", "Transport", 0, None, None),
    (60, "aeroporti di roma", "Travel", 0, None, None),
    (60, "narita airport", "Travel", 0, None, None),
    (60, "avolta", "Travel", 0, None, None),
    (60, "westin", "Travel", 0, None, None),
    (72, "royal mail", "Household", 0, None, None),
    (72, "post office", "Household", 0, None, None),
    (72, "fedex", "Household", 0, None, None),
    (72, "mail boxes", "Household", 0, None, None),
    (50, "e.leclerc", "Groceries", 0, None, None),
    (50, "edeka", "Groceries", 0, None, None),
    (50, "monoprix", "Groceries", 0, None, None),
    (50, "monop mark", "Groceries", 0, None, None),
    (50, "carrefour", "Groceries", 0, None, None),
    (50, "まいばすけっと", "Groceries", 0, None, None),
    (50, "marks and spencer", "Groceries", 0, None, None),
    (50, "circle k ", "Groceries", 0, None, None),
    (50, "seven-eleven", "Groceries", 0, None, None),
    (50, "7 eleven", "Groceries", 0, None, None),
    (75, "abercrombie", "Shopping", 0, None, None),
    (75, "paul smith", "Shopping", 0, None, None),
    (75, "h & m", "Shopping", 0, None, None),
    (75, "american eagle", "Shopping", 0, None, None),
    (75, "ralph lauren", "Shopping", 0, None, None),
    (75, "log-on", "Shopping", 0, None, None),
    (75, "eslite", "Shopping", 0, None, None),
    (75, "fotomax", "Shopping", 0, None, None),
    (75, "apple store", "Shopping", 0, None, None),
    (75, "flower delivery", "Shopping", 0, None, None),
    (75, "=mango", "Shopping", 0, None, None),
    (75, "intimissimi", "Shopping", 0, None, None),
    (75, "pretty ballerinas", "Shopping", 0, None, None),
    (75, "=upim", "Shopping", 0, None, None),
    (75, "la rinascente", "Shopping", 0, None, None),
    (75, "gioielleria", "Shopping", 0, None, None),
    (75, "sofmap", "Shopping", 0, None, None),
    (75, "primark", "Shopping", 0, None, None),
    (75, "whsmith", "Shopping", 0, None, None),
    (75, "oliverbonas", "Shopping", 0, None, None),
    (75, "knomo", "Shopping", 0, None, None),
    (75, "oneplus", "Shopping", 0, None, None),
    (75, "amznmktplace", "Shopping", 0, None, None),
    (75, "microsoft store", "Shopping", 0, None, None),
    (75, "mondadori", "Shopping", 0, None, None),
    (75, "=relay", "Shopping", 0, None, None),
    (75, "duty free", "Shopping", 0, None, None),
    (75, "poundland", "Shopping", 0, None, None),
    (75, "trezor.io", "Shopping", 0, None, None),
    (76, "mannings", "Health", 0, None, None),
    (76, "matsumoto kiyoshi", "Health", 0, None, None),
    (76, "=boots", "Health", 0, None, None),
    (77, "lush ", "Personal care", 0, None, None),
    (77, "bath and body works", "Personal care", 0, None, None),
    (77, "qb house", "Personal care", 0, None, None),
    (77, "nuxe", "Personal care", 0, None, None),
    (77, "sabon uk", "Personal care", 0, None, None),
    (77, "rossmann", "Personal care", 0, None, None),
    (77, "gillette.co.uk", "Personal care", 0, None, None),
    (77, "nail & spa", "Personal care", 0, None, None),
    (80, "bakery", "Dining", 0, None, None),
    (80, "auntie anne", "Dining", 0, None, None),
    (80, "bakehouse", "Dining", 0, None, None),
    (80, "banh mi", "Dining", 0, None, None),
    (80, "beard papa", "Dining", 0, None, None),
    (80, "curry house coc", "Dining", 0, None, None),
    (80, "curry hse coco", "Dining", 0, None, None),
    (80, "ichiran", "Dining", 0, None, None),
    (80, "ippudo", "Dining", 0, None, None),
    (80, "maison kayser", "Dining", 0, None, None),
    (80, "marugame", "Dining", 0, None, None),
    (80, "mrs field", "Dining", 0, None, None),
    (80, "musubi", "Dining", 0, None, None),
    (80, "pepper lunch", "Dining", 0, None, None),
    (80, "shake shack", "Dining", 0, None, None),
    (80, "congee", "Dining", 0, None, None),
    (80, "openrice", "Dining", 0, None, None),
    (80, "food panda", "Dining", 0, None, None),
    (80, "venchi", "Dining", 0, None, None),
    (80, "eggslut", "Dining", 0, None, None),
    (80, "saboten", "Dining", 0, None, None),
    (80, "maxim's", "Dining", 0, None, None),
    (80, "caffe", "Dining", 0, None, None),
    (80, "caffè", "Dining", 0, None, None),
    (80, "cafè", "Dining", 0, None, None),
    (80, "falafel", "Dining", 0, None, None),
    (80, "boulanger", "Dining", 0, None, None),
    (80, "enoteca", "Dining", 0, None, None),
    (80, "marche mövenpick", "Dining", 0, None, None),
    (80, "airport f & b", "Dining", 0, None, None),
    (80, "chiosco", "Dining", 0, None, None),
    (80, "gelato", "Dining", 0, None, None),
    (80, "piadineria", "Dining", 0, None, None),
    (80, "bäckerei", "Dining", 0, None, None),
    (80, "=areas", "Dining", 0, None, None),
    (80, "=illy", "Dining", 0, None, None),
    (80, "wok to walk", "Dining", 0, None, None),
    (80, "wendys", "Dining", 0, None, None),
    (80, "sweetgreen", "Dining", 0, None, None),
    (80, "boutique angelina", "Dining", 0, None, None),
];

pub const UNCATEGORIZED: &str = "Other";

/// One loaded rule, in match order.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: i64,
    pub priority: i64,
    pub pattern: String,
    pub category: String,
    pub is_transfer: i64,
    pub direction: Option<String>,
    pub share: Option<f64>,
    /// seed, user or suggested ("user" before the Phase 3 columns)
    pub origin: String,
}

/// Seed category_rules from DEFAULT_RULES once (if the table is empty). On a
/// table with the Phase 3 columns they go in as seed rules (after every user
/// rule in the effective order), and their categories join the table.
pub fn ensure_rules(conn: &Connection) -> Result<()> {
    let n: i64 = conn.query_row("SELECT COUNT(*) c FROM category_rules", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    let migrated = crate::categories::rules_migrated(conn);
    // a managed table that was seeded once and emptied by its owner stays
    // empty ("starting from zero" is a choice, not a fresh install)
    if migrated && crate::categories::meta(conn, crate::categories::RULES_SEEDED).is_some() {
        return Ok(());
    }
    let seed = |c: &Connection| -> Result<()> {
        {
            let sql = if migrated {
                "INSERT INTO category_rules(priority,pattern,category,is_transfer,direction,share,origin,created_at) VALUES(?,?,?,?,?,?,'seed',?)"
            } else {
                "INSERT INTO category_rules(priority,pattern,category,is_transfer,direction,share) VALUES(?,?,?,?,?,?)"
            };
            let mut st = c.prepare(sql)?;
            let now = crate::db::now_ms();
            for (pr, pat, cat, tr, dir, share) in DEFAULT_RULES {
                if migrated {
                    st.execute(params![pr, pat, cat, tr, dir, share, now])?;
                } else {
                    st.execute(params![pr, pat, cat, tr, dir, share])?;
                }
            }
        }
        if migrated {
            crate::categories::set_meta(c, crate::categories::RULES_SEEDED, "1")?;
        }
        crate::categories::sync_categories(c)?;
        Ok(())
    };
    // inside a caller's transaction (a write through rules::write) or its own
    if conn.is_autocommit() {
        let tx = conn.unchecked_transaction()?;
        seed(&tx)?;
        tx.commit()?;
    } else {
        seed(conn)?;
    }
    Ok(())
}

/// The ORDER BY of the effective order: user (and suggested) rules, then
/// seed rules, each by (priority, id). Plain (priority, id) before Phase 3.
pub fn order_by(migrated: bool) -> &'static str {
    if migrated {
        "ORDER BY (origin = 'seed'), priority, id"
    } else {
        "ORDER BY priority, id"
    }
}

/// The enabled rules in the effective order.
pub fn load_rules(conn: &Connection) -> Result<Vec<Rule>> {
    let migrated = crate::categories::rules_migrated(conn);
    let sql = if migrated {
        format!("SELECT id, priority, pattern, category, is_transfer, direction, share, origin FROM category_rules WHERE enabled = 1 {}", order_by(true))
    } else {
        format!("SELECT id, priority, pattern, category, is_transfer, direction, share, 'user' FROM category_rules {}", order_by(false))
    };
    let mut st = conn.prepare(&sql)?;
    let rules = st
        .query_map([], |r| {
            Ok(Rule {
                id: r.get(0)?,
                priority: r.get(1)?,
                pattern: r.get(2)?,
                category: r.get(3)?,
                is_transfer: r.get(4)?,
                direction: r.get(5)?,
                share: r.get(6)?,
                origin: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rules)
}

/// Whether `rule` fires on a row (`lower_desc` lowercased): direction
/// first, then the pattern.
pub fn rule_hits(rule: &Rule, lower_desc: &str, amount: f64) -> bool {
    pattern_hits(&rule.pattern, rule.direction.as_deref(), lower_desc, amount)
}

/// Whether a (pattern, direction) fires on a row (`lower_desc` lowercased).
pub fn pattern_hits(pattern: &str, direction: Option<&str>, lower_desc: &str, amount: f64) -> bool {
    match direction {
        Some("in") if !(amount > 0.0) => return false,
        Some("out") if !(amount < 0.0) => return false,
        _ => {}
    }
    match pattern.strip_prefix('=') {
        Some(whole) => textfmt::strip(lower_desc) == whole,
        None => lower_desc.contains(pattern),
    }
}

/// Index of the first matching rule.
pub fn match_index(desc: &str, amount: f64, rules: &[Rule]) -> Option<usize> {
    let d = desc.to_lowercase();
    rules.iter().position(|r| rule_hits(r, &d, amount))
}

/// (category, share) of the first matching rule; share None = the whole row.
pub fn match_rule<'a>(desc: &str, amount: f64, rules: &'a [Rule]) -> (&'a str, Option<f64>) {
    match match_index(desc, amount, rules) {
        Some(i) => (&rules[i].category, rules[i].share),
        None => (UNCATEGORIZED, None),
    }
}

/// (source, raw_hash) -> (category, share) of every override.
pub fn load_overrides(conn: &Connection) -> Result<HashMap<(String, String), (String, Option<f64>)>> {
    let mut overrides = HashMap::new();
    let mut st = conn.prepare("SELECT source, raw_hash, category, share FROM txn_overrides")?;
    let mut q = st.query([])?;
    while let Some(r) = q.next()? {
        overrides.insert((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?));
    }
    Ok(overrides)
}

/// (Re)assign bank_transactions.category (and share) from overrides, then
/// rules. Returns per-category counts in first-seen order.
pub fn categorize_all(conn: &Connection) -> Result<Vec<(String, usize)>> {
    ensure_rules(conn)?;
    let rules = load_rules(conn)?;
    let overrides = load_overrides(conn)?;
    let rows: Vec<(i64, String, String, Option<String>, Option<f64>)> = {
        let mut st = conn.prepare("SELECT id, source, raw_hash, description, amount FROM bank_transactions")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?;
        v
    };
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut slot: HashMap<String, usize> = HashMap::new();
    let tx = conn.unchecked_transaction()?;
    {
        let mut up = tx.prepare("UPDATE bank_transactions SET category=?, share=? WHERE id=?")?;
        for (id, source, raw_hash, desc, amount) in rows {
            let (cat, share) = match overrides.get(&(source, raw_hash)) {
                Some((c, s)) => (c.as_str(), *s),
                None => match_rule(desc.as_deref().unwrap_or(""), amount.unwrap_or(0.0), &rules),
            };
            up.execute(params![cat, share, id])?;
            let i = *slot.entry(cat.to_string()).or_insert_with(|| {
                counts.push((cat.to_string(), 0));
                counts.len() - 1
            });
            counts[i].1 += 1;
        }
    }
    tx.commit()?;
    Ok(counts)
}

/// categorize_all writing only the rows whose category or share moves, and
/// returning those (what a write through the category APIs replies). Runs
/// inside the caller's transaction (no transaction of its own).
pub fn recategorize(conn: &Connection) -> Result<Vec<crate::categories::Changed>> {
    ensure_rules(conn)?;
    let rules = load_rules(conn)?;
    let overrides = load_overrides(conn)?;
    type Row = (i64, String, String, Option<String>, Option<f64>, Option<String>, Option<f64>);
    let rows: Vec<Row> = {
        let mut st = conn.prepare("SELECT id, source, raw_hash, description, amount, category, share FROM bank_transactions")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
            .collect::<rusqlite::Result<_>>()?;
        v
    };
    let mut out = Vec::new();
    let mut up = conn.prepare("UPDATE bank_transactions SET category=?, share=? WHERE id=?")?;
    for (id, source, raw_hash, desc, amount, old_cat, old_share) in rows {
        let key = (source, raw_hash);
        let (cat, share) = match overrides.get(&key) {
            Some((c, s)) => (c.as_str(), *s),
            None => match_rule(desc.as_deref().unwrap_or(""), amount.unwrap_or(0.0), &rules),
        };
        if old_cat.as_deref() != Some(cat) || old_share != share {
            up.execute(params![cat, share, id])?;
            out.push(crate::categories::Changed { source: key.0, raw_hash: key.1, category: cat.to_string(), share });
        }
    }
    Ok(out)
}

/// The row an override acts on, as `set_override` returns it.
#[derive(Debug, Clone, PartialEq)]
pub struct TxnRow {
    pub id: i64,
    pub txn_date: String,
    pub source: String,
    pub amount: Option<f64>,
    pub currency: String,
    pub description: String,
    pub category: Option<String>,
    pub share: Option<f64>,
}

/// Pin one transaction's category (and share); category None removes the
/// override so the rules apply again. Recategorizes, returns the row.
pub fn set_override(
    conn: &Connection,
    txn_id: i64,
    category: Option<&str>,
    share: Option<f64>,
    note: Option<&str>,
) -> Result<TxnRow> {
    let row: Option<(String, String)> = conn
        .query_row("SELECT source, raw_hash FROM bank_transactions WHERE id=?", [txn_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((source, raw_hash)) = row else {
        bail!("no bank transaction with id {txn_id}");
    };
    match category {
        None => {
            conn.execute("DELETE FROM txn_overrides WHERE source=? AND raw_hash=?", params![source, raw_hash])?;
        }
        Some(cat) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            conn.execute(
                "INSERT OR REPLACE INTO txn_overrides(source,raw_hash,category,share,note,created_at) \
                 VALUES(?,?,?,?,?,?)",
                params![source, raw_hash, cat, share, note, now],
            )?;
        }
    }
    categorize_all(conn)?;
    let r = conn.query_row(
        "SELECT id, txn_date, source, amount, currency, description, category, share \
         FROM bank_transactions WHERE id=?",
        [txn_id],
        |r| {
            Ok(TxnRow {
                id: r.get(0)?,
                txn_date: r.get(1)?,
                source: r.get(2)?,
                amount: r.get(3)?,
                currency: r.get(4)?,
                description: r.get(5)?,
                category: r.get(6)?,
                share: r.get(7)?,
            })
        },
    )?;
    Ok(r)
}

/// Categories with an is_transfer rule (the pre-Phase-3 transfer test).
pub fn rule_transfer_categories(conn: &Connection) -> Result<HashSet<String>> {
    ensure_rules(conn)?;
    let mut st = conn.prepare("SELECT DISTINCT category FROM category_rules WHERE is_transfer=1")?;
    let set = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(set)
}

/// Categories that represent money moving between own accounts (not
/// spending): the categories table's non-spending kinds where it knows the
/// category, else the categories of is_transfer rules.
pub fn transfer_categories(conn: &Connection) -> Result<HashSet<String>> {
    Ok(crate::categories::Flags::load(conn).transfer_set())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DEFAULT_RULES as categorize_all loads them: by priority, then id.
    fn defaults() -> Vec<Rule> {
        let mut v: Vec<_> = DEFAULT_RULES.iter().enumerate().collect();
        v.sort_by_key(|(i, r)| (r.0, *i));
        v.into_iter()
            .map(|(i, r)| Rule {
                id: i as i64 + 1,
                priority: r.0,
                origin: "seed".into(),
                pattern: r.1.to_string(),
                category: r.2.to_string(),
                is_transfer: r.3,
                direction: r.4.map(str::to_string),
                share: r.5,
            })
            .collect()
    }

    #[test]
    fn default_rules_pick_the_right_category() {
        let rules = defaults();
        let cat = |d: &str, a: f64| match_rule(d, a, &rules).0.to_string();
        // N26 Instant Savings interest posts with an empty description: the
        // inflow is interest, the outflows the withholding tax on it
        assert_eq!(cat("", 6.78), "Interest / rebates");
        assert_eq!(cat("  ", -1.7), "Taxes");
        // a cash machine inside a Tesco is cash, not groceries
        assert_eq!(cat("Cash at Tesco Metro", -20.0), "Cash / ATM");
        assert_eq!(cat("The Cooperative Food", -20.0), "Groceries");
        // "park" no longer catches any word containing it
        assert_eq!(cat("PARKNSHOP 101XYZ SHA TIN HK", -50.0), "Groceries");
        assert_eq!(cat("Sparkle Ginzaten", -3.0), UNCATEGORIZED);
        assert_eq!(cat("PayPal Europe S.a.r.l. et Cie S.C.A", -0.99), "Subscriptions");
        assert_eq!(cat("BBMSL * M099 Marugame Hong Kong HK", -60.0), "Dining");
        assert_eq!(cat("MTR-KOWLOON TONG 01234 HONG KONG HK", -90.0), "Transport");
        assert_eq!(cat("Walmart Bakery FX Rate £1 = $1.2700", -5.0), "Groceries");
        // exact patterns don't fire on longer descriptions
        assert_eq!(cat("Mango", -30.0), "Shopping");
        assert_eq!(cat("Mango Tree Thai", -30.0), UNCATEGORIZED);
    }
}
