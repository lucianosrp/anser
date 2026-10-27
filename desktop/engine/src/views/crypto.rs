//! Crypto profit / holdings math (plus the crypto export load): DCA profit (DCA-only and all-in), monthly
//! invested vs value, the transfer-proof monthly market rows behind the
//! Investments TWR, and holdings per exchange.

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::{json, Map, Value};

use super::{get_f64, get_str, month_date, ms_to_ym, next_month, num, r2, ym_of};

pub const ASSETS: [&str; 3] = ["BTC", "ETH", "BNB"];
/// stable-funded buys
const BUY_KINDS: [&str; 4] = ["convert-in", "auto-invest", "fiat", "p2p-buy"];
const SELL_ALL: [&str; 4] = ["convert-out", "spot-sell", "p2p-sell", "fiat-sell"];

/// BUY_ALL: every buy that is money put in (stable-funded + spot buys).
pub fn is_buy(kind: &str) -> bool {
    BUY_KINDS.contains(&kind) || kind == "spot-buy"
}

fn is_stack(kind: &str) -> bool {
    is_buy(kind) || kind == "reward"
}

fn is_sell(kind: &str) -> bool {
    SELL_ALL.contains(&kind)
}

/// One crypto_events row: [ts, asset, delta, usd, kind, detail, exchange].
#[derive(Clone, Debug)]
pub struct Event {
    pub ts: i64,
    pub asset: String,
    pub delta: f64,
    pub usd: Option<f64>,
    pub kind: String,
    pub exchange: String,
}

pub struct Export {
    pub events: Vec<Event>,
    /// asset -> 'YYYY-MM' -> close
    pub prices_monthly: HashMap<String, HashMap<String, f64>>,
}

/// The crypto ledger and monthly closes.
pub fn load_export(conn: &Connection) -> Export {
    let events = conn
        .prepare_cached("SELECT ts, asset, delta, usd, kind, exchange FROM crypto_events ORDER BY ts")
        .and_then(|mut s| {
            s.query_map([], |r| {
                Ok(Event {
                    ts: r.get(0)?,
                    asset: get_str(r, 1).unwrap_or_default(),
                    delta: get_f64(r, 2).unwrap_or(0.0),
                    usd: get_f64(r, 3),
                    kind: get_str(r, 4).unwrap_or_default(),
                    exchange: get_str(r, 5).unwrap_or_default(),
                })
            })?
            .collect()
        })
        .unwrap_or_default();
    let mut pm: HashMap<String, HashMap<String, f64>> = HashMap::new();
    if let Ok(mut s) = conn.prepare_cached("SELECT asset, month, close FROM crypto_prices_monthly") {
        if let Ok(rows) = s.query_map([], |r| Ok((get_str(r, 0).unwrap_or_default(), get_str(r, 1).unwrap_or_default(), get_f64(r, 2)))) {
            for (a, m, c) in rows.flatten() {
                pm.entry(a).or_default().insert(m, c.unwrap_or(0.0));
            }
        }
    }
    Export { events, prices_monthly: pm }
}

/// cash_flow_usd: net money one event puts INTO crypto (a buy or a deposit
/// from elsewhere at its deposit-date cost, minus a sell's proceeds).
pub fn cash_flow_usd(kind: &str, delta: f64, usd: Option<f64>) -> f64 {
    let Some(usd) = usd.filter(|u| *u != 0.0) else { return 0.0 };
    if delta > 0.0 && (is_buy(kind) || kind == "deposit") {
        usd
    } else if delta < 0.0 && is_sell(kind) {
        -usd
    } else {
        0.0
    }
}

/// An insertion-ordered float map (missing keys read as 0).
#[derive(Clone, Debug, Default)]
struct OMap<K: PartialEq + Clone>(Vec<(K, f64)>);

impl<K: PartialEq + Clone> OMap<K> {
    fn add(&mut self, k: &K, v: f64) {
        match self.0.iter_mut().find(|(x, _)| x == k) {
            Some((_, s)) => *s += v,
            None => self.0.push((k.clone(), v)),
        }
    }
    /// The value at k, inserting 0.0 when absent
    fn touch(&mut self, k: &K) -> f64 {
        match self.0.iter().find(|(x, _)| x == k) {
            Some((_, v)) => *v,
            None => {
                self.0.push((k.clone(), 0.0));
                0.0
            }
        }
    }
    fn get(&self, k: &K) -> f64 {
        self.0.iter().find(|(x, _)| x == k).map(|(_, v)| *v).unwrap_or(0.0)
    }
    fn sum(&self) -> f64 {
        self.0.iter().fold(0.0, |a, (_, v)| a + v)
    }
}

pub struct MonthRow {
    pub month: String,
    /// None = the JSON integer 0 (a sum over nothing)
    pub total_invested: Option<f64>,
    pub total_value: Option<f64>,
    pub btc_coins: f64,
    pub btc_invested: f64,
    pub btc_dep_coins: f64,
    pub btc_dep_cost: f64,
    pub btc_price: f64,
}

impl MonthRow {
    fn fields(&self, m: &mut Map<String, Value>) {
        let n0 = |v: Option<f64>| v.map(num).unwrap_or(json!(0));
        m.insert("month".into(), json!(self.month));
        m.insert("total_invested".into(), n0(self.total_invested));
        m.insert("total_value".into(), n0(self.total_value));
        m.insert("BTC_coins".into(), num(self.btc_coins));
        m.insert("BTC_invested".into(), num(self.btc_invested));
        m.insert("BTC_dep_coins".into(), num(self.btc_dep_coins));
        m.insert("BTC_dep_cost".into(), num(self.btc_dep_cost));
        m.insert("BTC_price".into(), num(self.btc_price));
    }

    pub fn json(&self) -> Value {
        let mut m = Map::new();
        self.fields(&mut m);
        Value::Object(m)
    }
}

pub struct MarketRow {
    pub month: String,
    pub v0: f64,
    pub v1_traded: f64,
    pub flow: f64,
    pub priced: bool,
}

pub struct Profit {
    pub invested: f64,
    pub bought: f64,
    pub realized: f64,
    pub owned: f64,
    pub value: f64,
    pub profit: f64,
    pub roi: f64,
    pub price: f64,
    pub avg: f64,
    pub deposited: f64,
    pub withdrawn: f64,
    pub invested_allin: f64,
    pub owned_allin: f64,
    pub value_allin: f64,
    pub profit_allin: f64,
    pub roi_allin: f64,
    pub avg_allin: f64,
}

pub struct Computed {
    pub rows: Vec<MonthRow>,
    pub market_rows: Vec<MarketRow>,
    pub spend_rows: Vec<Value>,
    pub profit: Vec<(String, Profit)>,
    pub held: Vec<(String, f64)>,
    pub totals: Value,
    pub price: Vec<(String, f64)>,
}

/// Everything the crypto page renders. None for an
/// empty ledger (`{"empty": true}`). `cur_month` is the running month (the
/// wall clock's, not utc_today(); the same day
/// unless FINANCE_TODAY pins another).
pub fn compute(export: &Export, price: &[(String, f64)], cur_month: &str) -> Option<Computed> {
    let assets: Vec<String> = ASSETS.iter().map(|a| a.to_string()).collect();
    let mut events: Vec<&Event> = export.events.iter().collect();
    events.sort_by_key(|e| e.ts); // stable, like sorted(key=e[0])
    if events.is_empty() {
        return None;
    }
    let live = |a: &str| price.iter().find(|(k, _)| k == a).map(|(_, p)| *p).unwrap_or(0.0);

    let first = events.iter().map(|e| ms_to_ym(e.ts)).min().unwrap();
    let mut all_months: Vec<String> = Vec::new();
    let (mut m, end) = (month_date(&first), month_date(cur_month));
    while m <= end {
        all_months.push(ym_of(m));
        m = next_month(m);
    }
    // price_at(a, month index): the live price in the running month, else
    // the last monthly close at or before it
    let pmon_ff: HashMap<&str, Vec<f64>> = ASSETS
        .iter()
        .map(|a| {
            let mut last = 0.0;
            let series = all_months
                .iter()
                .map(|mm| {
                    if let Some(c) = export.prices_monthly.get(*a).and_then(|p| p.get(mm)) {
                        last = *c;
                    }
                    last
                })
                .collect();
            (*a, series)
        })
        .collect();
    let price_at = |a: &str, i: usize| -> f64 {
        if all_months[i] == cur_month {
            live(a)
        } else {
            pmon_ff.get(a).map(|s| s[i]).unwrap_or(0.0)
        }
    };

    let mut invested: OMap<String> = OMap::default();
    let mut net: OMap<(String, String)> = OMap::default();
    let mut stack: OMap<String> = OMap::default();
    let mut depc: OMap<String> = OMap::default();
    let mut depq: OMap<String> = OMap::default();
    let mut rows = Vec::with_capacity(all_months.len());
    let mut market_rows = Vec::with_capacity(all_months.len());
    let mut prev: Option<usize> = None;
    let mut ei = 0;
    for (mi, m) in all_months.iter().enumerate() {
        let mut q0: OMap<String> = OMap::default();
        for ((_, a), v) in &net.0 {
            q0.add(a, v.max(0.0));
        }
        let mut traded: OMap<String> = OMap::default();
        let mut flow_usd = 0.0;
        while ei < events.len() && ms_to_ym(events[ei].ts).as_str() <= m.as_str() {
            let e = events[ei];
            ei += 1;
            if !assets.contains(&e.asset) {
                continue;
            }
            let cf = cash_flow_usd(&e.kind, e.delta, e.usd);
            if cf != 0.0 || e.kind == "reward" {
                traded.add(&e.asset, e.delta);
                flow_usd += cf;
            }
            net.add(&(e.exchange.clone(), e.asset.clone()), e.delta);
            if e.delta > 0.0 {
                if is_stack(&e.kind) {
                    stack.add(&e.asset, e.delta);
                }
                if is_buy(&e.kind) {
                    if let Some(u) = e.usd.filter(|u| *u != 0.0) {
                        invested.add(&e.asset, u);
                    }
                }
                if e.kind == "deposit" {
                    depq.add(&e.asset, e.delta);
                    depc.add(&e.asset, e.usd.unwrap_or(0.0));
                }
            }
        }
        let btc = "BTC".to_string();
        let total_invested = (!invested.0.is_empty()).then(|| r2(invested.sum()));
        let total_value = (!net.0.is_empty())
            .then(|| r2(net.0.iter().fold(0.0, |acc, ((_, a), v)| acc + v.max(0.0) * price_at(a, mi))));
        rows.push(MonthRow {
            month: m.clone(),
            total_invested,
            total_value,
            btc_coins: stack.touch(&btc),
            btc_invested: invested.touch(&btc),
            btc_dep_coins: depq.touch(&btc),
            btc_dep_cost: depc.touch(&btc),
            btc_price: r2(price_at("BTC", mi)),
        });
        let v0 = match prev {
            Some(p) => q0.0.iter().fold(0.0, |acc, (a, q)| acc + q * price_at(a, p)),
            None => 0.0,
        };
        let mut involved: Vec<&String> = q0.0.iter().map(|(a, _)| a).collect();
        for (a, _) in &traded.0 {
            if !involved.contains(&a) {
                involved.push(a);
            }
        }
        let v1_traded =
            involved.iter().fold(0.0, |acc, a| acc + (q0.get(a) + traded.get(a)).max(0.0) * price_at(a, mi));
        let mut check: Vec<&String> = q0.0.iter().filter(|(_, q)| *q >= 1e-8).map(|(a, _)| a).collect();
        for (a, d) in &traded.0 {
            if d.abs() >= 1e-8 && !check.contains(&a) {
                check.push(a);
            }
        }
        let priced = check.iter().all(|a| {
            price_at(a, mi) > 0.0 && (q0.get(a) < 1e-8 || prev.is_some_and(|p| price_at(a, p) > 0.0))
        });
        market_rows.push(MarketRow { month: m.clone(), v0, v1_traded, flow: flow_usd, priced });
        prev = Some(mi);
    }

    // spend bars: every buy, per month
    let mut spend: std::collections::BTreeMap<String, OMap<String>> = std::collections::BTreeMap::new();
    for e in &events {
        if is_buy(&e.kind) && e.delta > 0.0 && assets.contains(&e.asset) {
            if let Some(u) = e.usd.filter(|u| *u != 0.0) {
                spend.entry(ms_to_ym(e.ts)).or_default().add(&e.asset, u);
            }
        }
    }
    let spend_rows: Vec<Value> = spend
        .iter_mut()
        .map(|(m, s)| {
            let mut o = Map::new();
            o.insert("month".into(), json!(m));
            for a in &assets {
                o.insert(a.clone(), num(s.touch(a)));
            }
            o.insert("total".into(), num(s.sum()));
            Value::Object(o)
        })
        .collect();

    // all-time profit (DCA-only + all-in)
    #[derive(Default, Clone, Copy)]
    struct Acc {
        inv: f64,
        bought: f64,
        proceeds: f64,
        sold: f64,
        dep: f64,
        dep_cost: f64,
        wd: f64,
    }
    let mut acc: HashMap<&str, Acc> = HashMap::new();
    for e in &events {
        if !assets.contains(&e.asset) {
            continue;
        }
        let a = acc.entry(e.asset.as_str()).or_default();
        if is_buy(&e.kind) && e.delta > 0.0 {
            a.bought += e.delta;
            if let Some(u) = e.usd.filter(|u| *u != 0.0) {
                a.inv += u;
            }
        } else if is_sell(&e.kind) && e.delta < 0.0 {
            a.sold += -e.delta;
            if let Some(u) = e.usd.filter(|u| *u != 0.0) {
                a.proceeds += u;
            }
        } else if e.kind == "deposit" {
            a.dep += e.delta;
            a.dep_cost += e.usd.unwrap_or(0.0);
        } else if e.kind == "withdraw" {
            a.wd += -e.delta;
        }
    }
    let mut profit = Vec::new();
    for a in ASSETS {
        let c = acc.get(a).copied().unwrap_or_default();
        let owned = (c.bought - c.sold).max(0.0);
        let dca_sold = c.sold.min(c.bought);
        let realized = if c.sold != 0.0 { c.proceeds * (dca_sold / c.sold) } else { 0.0 };
        let px = live(a);
        let val = owned * px;
        let pf = val + realized - c.inv;
        let inv_ai = c.inv + c.dep_cost;
        let owned_ai = owned + c.dep;
        let val_ai = owned_ai * px;
        let pf_ai = val_ai + realized - inv_ai;
        profit.push((a.to_string(), Profit {
            invested: c.inv,
            bought: c.bought,
            realized,
            owned,
            value: val,
            profit: pf,
            roi: if c.inv != 0.0 { pf / c.inv * 100.0 } else { 0.0 },
            price: px,
            avg: if c.bought != 0.0 { c.inv / c.bought } else { 0.0 },
            deposited: c.dep,
            withdrawn: c.wd,
            invested_allin: inv_ai,
            owned_allin: owned_ai,
            value_allin: val_ai,
            profit_allin: pf_ai,
            roi_allin: if inv_ai != 0.0 { pf_ai / inv_ai * 100.0 } else { 0.0 },
            avg_allin: if c.bought + c.dep != 0.0 { inv_ai / (c.bought + c.dep) } else { 0.0 },
        }));
    }
    let s = |f: fn(&Profit) -> f64| profit.iter().fold(0.0, |acc, (_, p)| acc + f(p));
    let (ti, tv, tr) = (s(|p| p.invested), s(|p| p.value), s(|p| p.realized));
    let tp = tv + tr - ti;
    let (ai_inv, ai_val) = (s(|p| p.invested_allin), s(|p| p.value_allin));
    let ai_tp = ai_val + tr - ai_inv;
    let totals = json!({
        "invested": num(ti), "value": num(tv), "realized": num(tr), "profit": num(tp),
        "roi": num(if ti != 0.0 { tp / ti * 100.0 } else { 0.0 }),
        "invested_allin": num(ai_inv), "value_allin": num(ai_val), "profit_allin": num(ai_tp),
        "roi_allin": num(if ai_inv != 0.0 { ai_tp / ai_inv * 100.0 } else { 0.0 }),
    });
    let held = ASSETS
        .iter()
        .map(|a| (a.to_string(), net.0.iter().filter(|((_, aa), _)| aa == a).fold(0.0, |acc, (_, v)| acc + v.max(0.0))))
        .collect();
    Some(Computed { rows, market_rows, spend_rows, profit, held, totals, price: price.to_vec() })
}

impl Profit {
    pub fn json(&self) -> Value {
        json!({"invested": num(self.invested), "bought": num(self.bought), "realized": num(self.realized),
               "owned": num(self.owned), "value": num(self.value), "profit": num(self.profit),
               "roi": num(self.roi), "price": num(self.price), "avg": num(self.avg),
               "deposited": num(self.deposited), "withdrawn": num(self.withdrawn),
               "invested_allin": num(self.invested_allin), "owned_allin": num(self.owned_allin),
               "value_allin": num(self.value_allin), "profit_allin": num(self.profit_allin),
               "roi_allin": num(self.roi_allin), "avg_allin": num(self.avg_allin)})
    }
}

fn price_json(price: &[(String, f64)]) -> Value {
    Value::Object(price.iter().map(|(k, v)| (k.clone(), num(*v))).collect())
}

impl Computed {
    /// The full compute() result (the crypto page).
    pub fn json(&self) -> Value {
        let btc_rows: Vec<Value> = self
            .rows
            .iter()
            .filter(|r| r.btc_coins > 0.0)
            .map(|r| {
                let coins = r.btc_coins;
                let aq = coins + r.btc_dep_coins;
                let ai = r.btc_invested + r.btc_dep_cost;
                let mut m = Map::new();
                r.fields(&mut m);
                m.insert("btc_avg".into(), num(r.btc_invested / coins));
                m.insert("btc_avg_allin".into(), num(if aq != 0.0 { ai / aq } else { r.btc_invested / coins }));
                Value::Object(m)
            })
            .collect();
        json!({
            "empty": false,
            "assets": ASSETS,
            "rows": self.rows.iter().map(MonthRow::json).collect::<Vec<_>>(),
            "market_rows": self.market_rows.iter().map(|r| json!({"month": r.month, "v0": num(r.v0),
                "v1_traded": num(r.v1_traded), "flow": num(r.flow), "priced": r.priced})).collect::<Vec<_>>(),
            "btc_rows": btc_rows,
            "spend_rows": self.spend_rows,
            "profit": Value::Object(self.profit.iter().map(|(a, p)| (a.clone(), p.json())).collect()),
            "held": Value::Object(self.held.iter().map(|(a, v)| (a.clone(), num(*v))).collect()),
            "totals": self.totals,
            "price": price_json(&self.price),
        })
    }

    /// The export bundle's crypto section: per held coin qty/value (every
    /// venue) next to the DCA notions (invested/avg/profit, *_allin adds
    /// deposits); dust below a satoshi is dropped.
    pub fn bundle_json(&self, stale: bool, as_of: Option<&str>) -> Value {
        let mut assets = Map::new();
        for (a, p) in &self.profit {
            let qty = self.held.iter().find(|(k, _)| k == a).map(|(_, v)| *v).unwrap_or(0.0);
            if qty < 1e-8 && p.owned_allin < 1e-8 {
                continue;
            }
            assets.insert(a.clone(), json!({
                "qty": num(qty), "price": num(p.price), "value": num(r2(qty * p.price)),
                "invested": num(r2(p.invested)), "avg": num(r2(p.avg)), "profit": num(r2(p.profit)),
                "roi": num(r2(p.roi)), "realized": num(r2(p.realized)),
                "invested_allin": num(r2(p.invested_allin)), "avg_allin": num(r2(p.avg_allin)),
                "profit_allin": num(r2(p.profit_allin)), "roi_allin": num(r2(p.roi_allin)),
                "bought": num(p.bought), "owned": num(p.owned), "owned_allin": num(p.owned_allin)}));
        }
        json!({"totals": self.totals, "price": price_json(&self.price),
               "rows": self.rows.iter().map(MonthRow::json).collect::<Vec<_>>(),
               "assets": assets, "prices_stale": stale, "prices_as_of": as_of})
    }
}
