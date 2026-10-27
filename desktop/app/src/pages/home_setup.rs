//! Settings › Home: the home setup (the `home` user setting): rent or none,
//! the monthly rent and its currency, the lease's dates, the landlord's text
//! and the one-off costs (a deposit, an agency fee, stamp duty). Spending's
//! Home tab reads it (finance_core::home::HomeSetup).
//!
//! The landlord text is not a second matcher: on save it becomes a rule
//! (text -> Rent) through the categories API, unless a rule with that text
//! exists already.

use std::rc::Rc;
use std::sync::Arc;

use finance_core::categories::{self as cats, RuleDraft};
use finance_core::home::{self, HomeSetup, OneOff, Suggested, Suggestion, ONEOFF_KINDS};
use finance_core::mortgage::{self, Third};
use finance_core::{user_settings, Ledger};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{json, Value};

use super::sources::json_of;
use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

/// A write the app must apply to its ledger (the new rule).
pub enum HomeSetupEvent {
    Written(Value),
}

impl EventEmitter<HomeSetupEvent> for HomeSetupForm {}

struct OneOffRow {
    kind: &'static str,
    amount: Entity<InputState>,
    date: Entity<InputState>,
    returned: Entity<InputState>,
}

/// A dated amount of the mortgage: an overpayment (date, amount) or a rate
/// change (from, rate).
struct Pair {
    when: Entity<InputState>,
    value: Entity<InputState>,
}

pub struct HomeSetupForm {
    /// "rent", "mortgage" or "none"
    mode: &'static str,
    principal: Entity<InputState>,
    rate: Entity<InputState>,
    term: Entity<InputState>,
    start: Entity<InputState>,
    pay_day: Entity<InputState>,
    lender: Entity<InputState>,
    home_value: Entity<InputState>,
    home_value_date: Entity<InputState>,
    overs: Vec<Pair>,
    changes: Vec<Pair>,
    monthly: Entity<InputState>,
    currency: Entity<InputState>,
    lease_start: Entity<InputState>,
    lease_end: Entity<InputState>,
    landlord: Entity<InputState>,
    oneoffs: Vec<OneOffRow>,
    ledger: Option<Arc<Ledger>>,
    /// the inputs took the saved setting once
    filled: bool,
    /// which suggestion the placeholders show (the memoized one's address)
    placeholders_for: usize,
    busy: bool,
    /// (ok, text)
    note: Option<(bool, String)>,
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "deposit" => "Deposit",
        "agency_fee" => "Agency fee",
        "legal_fee" => "Legal fee",
        _ => "Stamp duty",
    }
}

/// A field the suggestions can fill.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Monthly,
    Currency,
    LeaseStart,
    LeaseEnd,
    Landlord,
    LoanStart,
    Lender,
    Principal,
    Rate,
    Term,
}

struct Fill {
    target: Target,
    value: String,
    why: String,
}

/// What "Use suggestions" would do: the empty fields with a suggestion, the
/// mode (only before anything is saved), and the cost rows (only while there
/// are none).
struct Plan {
    mode: Option<&'static str>,
    fills: Vec<Fill>,
    oneoffs: Vec<Suggested<OneOff>>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.mode.is_none() && self.fills.is_empty() && self.oneoffs.is_empty()
    }

    fn why(&self, t: Target) -> Option<&str> {
        self.fills.iter().find(|f| f.target == t).map(|f| f.why.as_str())
    }
}

fn number(v: f64) -> String {
    format!("{v}")
}

/// The one-off kinds each mode offers.
fn kinds_for(mode: &str) -> &'static [&'static str] {
    if mode == "mortgage" { &["stamp_duty", "legal_fee", "agency_fee"] } else { &["deposit", "agency_fee", "stamp_duty"] }
}

fn kind_of(s: &str) -> &'static str {
    ONEOFF_KINDS.iter().copied().find(|k| *k == s).unwrap_or("deposit")
}

impl HomeSetupForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        let f = HomeSetupForm {
            mode: "rent",
            principal: input("Amount borrowed", window, cx),
            rate: input("Yearly rate, %", window, cx),
            term: input("Months, e.g. 300", window, cx),
            start: input("YYYY-MM-DD", window, cx),
            pay_day: input("1-28, optional", window, cx),
            lender: input("Text on the lender's payments", window, cx),
            home_value: input("Optional", window, cx),
            home_value_date: input("YYYY-MM-DD", window, cx),
            overs: Vec::new(),
            changes: Vec::new(),
            monthly: input("Monthly rent, e.g. 1250", window, cx),
            currency: input("HKD", window, cx),
            lease_start: input("Start YYYY-MM-DD", window, cx),
            lease_end: input("End YYYY-MM-DD", window, cx),
            landlord: input("Text on the landlord's payments", window, cx),
            oneoffs: Vec::new(),
            ledger: None,
            filled: false,
            placeholders_for: 0,
            busy: false,
            note: None,
        };
        // the currency field shows the profile's base currency until typed over
        f.currency.update(cx, |s, cx| s.set_placeholder(fmt::base_ccy(), window, cx));
        f
    }

    /// The ledger, for the rule check (a rule with that text may exist).
    pub fn set_ledger(&mut self, ledger: Arc<Ledger>, cx: &mut Context<Self>) {
        self.ledger = Some(ledger);
        cx.notify();
    }

    fn new_row(&self, o: Option<&OneOff>, kind: &str, window: &mut Window, cx: &mut Context<Self>) -> OneOffRow {
        let input = |ph: &str, value: String, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(value))
        };
        let amount = o.filter(|o| o.amount > 0.).map(|o| format!("{}", o.amount)).unwrap_or_default();
        OneOffRow {
            kind: kind_of(kind),
            amount: input("Amount", amount, window, cx),
            date: input("Paid YYYY-MM-DD", o.and_then(|o| o.date.clone()).unwrap_or_default(), window, cx),
            returned: input("Returned YYYY-MM-DD", o.and_then(|o| o.returned_on.clone()).unwrap_or_default(), window, cx),
        }
    }

    fn new_pair(&self, ph: (&str, &str), vals: (String, String), window: &mut Window, cx: &mut Context<Self>) -> Pair {
        let input = |ph: &str, value: String, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(value))
        };
        Pair { when: input(ph.0, vals.0, window, cx), value: input(ph.1, vals.1, window, cx) }
    }

    /// The ledger's suggestions (kept until new data lands).
    fn suggestion(&self) -> Rc<Suggestion> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let ledger = self.ledger.clone();
        let id = ledger.as_ref().map(|l| Arc::as_ptr(l) as usize);
        crate::memo::get("home.suggest", &(today.clone(), id), move || ledger.map(|l| home::suggest(&l, &today)).unwrap_or_default())
    }

    /// What the suggestions would fill in now.
    fn plan(&self, sug: &Suggestion, saved: bool, cx: &App) -> Plan {
        let mode = match (&sug.mode, saved) {
            (Some(m), false) if m.value != self.mode => Some(m.value),
            _ => None,
        };
        let eff = mode.unwrap_or(self.mode);
        let mut fills = Vec::new();
        let mut add = |target: Target, input: &Entity<InputState>, s: Option<(String, &str)>| {
            if let Some((value, why)) = s.filter(|_| Self::text(input, cx).is_empty()) {
                fills.push(Fill { target, value, why: why.to_string() });
            }
        };
        fn s(x: &Option<Suggested<String>>) -> Option<(String, &str)> {
            x.as_ref().map(|x| (x.value.clone(), x.why.as_str()))
        }
        if sug.mode.as_ref().is_some_and(|m| m.value == eff) {
            add(Target::Currency, &self.currency, s(&sug.currency));
        }
        if eff == "rent" {
            add(Target::Monthly, &self.monthly, sug.rent_monthly.as_ref().map(|m| (number(m.value), m.why.as_str())));
            add(Target::LeaseStart, &self.lease_start, s(&sug.lease_start));
            add(Target::LeaseEnd, &self.lease_end, s(&sug.lease_end));
            add(Target::Landlord, &self.landlord, s(&sug.landlord));
        }
        if eff == "mortgage" {
            add(Target::LoanStart, &self.start, s(&sug.mortgage_start));
            add(Target::Lender, &self.lender, s(&sug.lender));
            // two of amount, rate and term and the payment give the third
            let typed = |i: &Entity<InputState>| Self::text(i, cx).replace([',', ' '], "").parse::<f64>().ok();
            let (p, r, n) = (typed(&self.principal), typed(&self.rate), typed(&self.term).map(|n| n.round() as u32));
            if let Some(pay) = &sug.mortgage_payment {
                let why = format!("what a payment of {} a month implies", fmt::amount_in(&fmt::base_ccy(), pay.value));
                match mortgage::third(p, r, n, pay.value) {
                    Some(Third::Term(m)) => add(Target::Term, &self.term, Some((m.to_string(), why.as_str()))),
                    Some(Third::Principal(v)) => add(Target::Principal, &self.principal, Some((format!("{}", v.round()), why.as_str()))),
                    Some(Third::Rate(v)) => add(Target::Rate, &self.rate, Some((number(v), why.as_str()))),
                    None => {}
                }
            }
        }
        let oneoffs = if self.oneoffs.is_empty() {
            sug.oneoffs.iter().filter(|o| kinds_for(eff).contains(&o.value.kind.as_str())).cloned().collect()
        } else {
            Vec::new()
        };
        Plan { mode, fills, oneoffs }
    }

    fn input_of(&self, t: Target) -> &Entity<InputState> {
        match t {
            Target::Monthly => &self.monthly,
            Target::Currency => &self.currency,
            Target::LeaseStart => &self.lease_start,
            Target::LeaseEnd => &self.lease_end,
            Target::Landlord => &self.landlord,
            Target::LoanStart => &self.start,
            Target::Lender => &self.lender,
            Target::Principal => &self.principal,
            Target::Rate => &self.rate,
            Target::Term => &self.term,
        }
    }

    /// "Use suggestions": every empty field with a suggestion takes it;
    /// typed values stay, and nothing is saved.
    fn use_suggestions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sug = self.suggestion();
        let saved = super::spending_home::setup().is_some();
        let plan = self.plan(&sug, saved, cx);
        if let Some(m) = plan.mode {
            self.mode = m;
            let kinds = kinds_for(m);
            for r in self.oneoffs.iter_mut().filter(|r| !kinds.contains(&r.kind)) {
                r.kind = kinds[0];
            }
        }
        for f in &plan.fills {
            self.input_of(f.target).update(cx, |i, cx| i.set_value(f.value.clone(), window, cx));
        }
        for o in &plan.oneoffs {
            let row = self.new_row(Some(&o.value), &o.value.kind, window, cx);
            self.oneoffs.push(row);
        }
        self.note = Some((true, "Filled in from your transactions: check the values. Nothing is saved until you press Save.".into()));
        cx.notify();
    }

    /// The empty fields show the suggestion as their placeholder.
    fn show_placeholders(&self, sug: &Suggestion, window: &mut Window, cx: &mut Context<Self>) {
        let set = |i: &Entity<InputState>, text: String, window: &mut Window, cx: &mut Context<Self>| i.update(cx, |i, cx| i.set_placeholder(text, window, cx));
        let text = |x: &Option<Suggested<String>>, default: &str| x.as_ref().map(|x| x.value.clone()).unwrap_or_else(|| default.to_string());
        set(&self.currency, text(&sug.currency, &fmt::base_ccy()), window, cx);
        set(&self.monthly, sug.rent_monthly.as_ref().map(|m| number(m.value)).unwrap_or_else(|| "Monthly rent, e.g. 1250".into()), window, cx);
        set(&self.lease_start, text(&sug.lease_start, "Start YYYY-MM-DD"), window, cx);
        set(&self.lease_end, text(&sug.lease_end, "End YYYY-MM-DD"), window, cx);
        set(&self.landlord, text(&sug.landlord, "Text on the landlord's payments"), window, cx);
        set(&self.start, text(&sug.mortgage_start, "YYYY-MM-DD"), window, cx);
        set(&self.lender, text(&sug.lender, "Text on the lender's payments"), window, cx);
    }

    /// Take the saved setting into the inputs, once it is known.
    fn fill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filled = true;
        self.currency.update(cx, |i, cx| i.set_placeholder(fmt::base_ccy(), window, cx));
        let Some(s) = super::spending_home::saved() else { return };
        self.mode = match s.mode.as_str() {
            "none" => "none",
            "mortgage" => "mortgage",
            _ => "rent",
        };
        let m = &s.mortgage;
        let num = |v: f64| if v > 0. { format!("{v}") } else { String::new() };
        let set_m = |i: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| i.update(cx, |i, cx| i.set_value(v, window, cx));
        set_m(&self.principal, num(m.principal), window, cx);
        set_m(&self.rate, if m.principal > 0. { format!("{}", m.annual_rate_pct) } else { String::new() }, window, cx);
        set_m(&self.term, if m.term_months > 0 { m.term_months.to_string() } else { String::new() }, window, cx);
        set_m(&self.start, m.start.clone().unwrap_or_default(), window, cx);
        set_m(&self.pay_day, m.payment_day.map(|d| d.to_string()).unwrap_or_default(), window, cx);
        set_m(&self.lender, m.lender_match.clone().unwrap_or_default(), window, cx);
        set_m(&self.home_value, m.home_value.map(num).unwrap_or_default(), window, cx);
        set_m(&self.home_value_date, m.home_value_date.clone().unwrap_or_default(), window, cx);
        self.overs = m.overpayments.iter().map(|o| self.new_pair(("YYYY-MM-DD", "Amount"), (o.date.clone().unwrap_or_default(), num(o.amount)), window, cx)).collect();
        self.changes = m.rate_changes.iter().map(|c| self.new_pair(("YYYY-MM-DD", "Yearly rate, %"), (c.from.clone().unwrap_or_default(), format!("{}", c.annual_rate_pct)), window, cx)).collect();
        let set = |i: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| i.update(cx, |i, cx| i.set_value(v, window, cx));
        set(&self.monthly, if s.rent.monthly > 0. { format!("{}", s.rent.monthly) } else { String::new() }, window, cx);
        set(&self.currency, s.currency.clone().unwrap_or_default(), window, cx);
        set(&self.lease_start, s.rent.lease.start.clone().unwrap_or_default(), window, cx);
        set(&self.lease_end, s.rent.lease.end.clone().unwrap_or_default(), window, cx);
        set(&self.landlord, s.rent.landlord_match.clone().unwrap_or_default(), window, cx);
        self.oneoffs = s.oneoffs.iter().map(|o| self.new_row(Some(o), &o.kind, window, cx)).collect();
    }

    fn text(i: &Entity<InputState>, cx: &App) -> String {
        i.read(cx).value().trim().to_string()
    }

    /// The form as the setting's JSON, or what is wrong with it.
    fn build(&self, cx: &App) -> Result<Value, String> {
        let opt = |s: String| if s.is_empty() { Value::Null } else { Value::String(s) };
        let num = |s: String, what: &str| -> Result<f64, String> {
            if s.is_empty() {
                return Ok(0.);
            }
            s.replace([',', ' '], "").replace(&fmt::sym(), "").parse::<f64>().map_err(|_| format!("{what} must be a number"))
        };
        let currency = Self::text(&self.currency, cx).to_uppercase();
        let mut v = json!({"mode": self.mode, "currency": if currency.is_empty() { fmt::base_ccy() } else { currency }});
        if self.mode == "rent" {
            v["rent"] = json!({
                "monthly": num(Self::text(&self.monthly, cx), "The monthly rent")?,
                "landlord_match": Self::text(&self.landlord, cx),
                "lease": {"start": opt(Self::text(&self.lease_start, cx)), "end": opt(Self::text(&self.lease_end, cx))},
            });
        }
        if self.mode == "mortgage" {
            let n = |i: &Entity<InputState>, what: &str| num(Self::text(i, cx), what);
            let day = Self::text(&self.pay_day, cx);
            let value = Self::text(&self.home_value, cx);
            let mut overs = Vec::new();
            for p in &self.overs {
                overs.push(json!({"date": opt(Self::text(&p.when, cx)), "amount": n(&p.value, "An overpayment")?}));
            }
            let mut changes = Vec::new();
            for p in &self.changes {
                changes.push(json!({"from": opt(Self::text(&p.when, cx)), "annual_rate_pct": n(&p.value, "A rate change")?}));
            }
            v["mortgage"] = json!({
                "principal": n(&self.principal, "The amount borrowed")?,
                "annual_rate_pct": n(&self.rate, "The rate")?,
                "term_months": n(&self.term, "The term")?.round() as u32,
                "start": opt(Self::text(&self.start, cx)),
                "payment_day": if day.is_empty() { Value::Null } else { json!(day.parse::<u32>().map_err(|_| "The payment day must be a number from 1 to 28".to_string())?) },
                "lender_match": Self::text(&self.lender, cx),
                "home_value": if value.is_empty() { Value::Null } else { json!(n(&self.home_value, "The home value")?) },
                "home_value_date": opt(Self::text(&self.home_value_date, cx)),
                "overpayments": overs,
                "rate_changes": changes,
            });
        }
        let mut one = Vec::new();
        for r in &self.oneoffs {
            one.push(json!({
                "kind": r.kind,
                "amount": num(Self::text(&r.amount, cx), "A cost's amount")?,
                "date": opt(Self::text(&r.date, cx)),
                "returned_on": opt(Self::text(&r.returned, cx)),
            }));
        }
        v["oneoffs"] = Value::Array(one);
        // reject what the server would, before sending
        HomeSetup::check(&v)?;
        Ok(v)
    }

    /// The rule the landlord's text needs, and the Rent category first when
    /// the ledger has none: None when there is no text or a rule has it.
    fn rule_plan(&self, text: &str, category: &str) -> Option<(Option<Value>, RuleDraft)> {
        let text = text.trim();
        let ledger = self.ledger.as_ref()?;
        if text.is_empty() || ledger.rules.iter().any(|r| r.pattern.trim().eq_ignore_ascii_case(text)) {
            return None;
        }
        // every category counts, hidden ones too: a create would be refused
        let create = home::create_if_missing(category, ledger.categories.iter().map(|c| c.name.as_str()).chain(ledger.entries.iter().map(|e| e.category.as_str())));
        Some((create, RuleDraft { pattern: text.to_string(), direction: Some("out".into()), category: category.into(), ..Default::default() }))
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let v = match self.build(cx) {
            Ok(v) => v,
            Err(e) => {
                self.note = Some((false, e));
                cx.notify();
                return;
            }
        };
        let plan = match self.mode {
            "rent" => self.rule_plan(&Self::text(&self.landlord, cx), "Rent"),
            "mortgage" => self.rule_plan(&Self::text(&self.lender, cx), "Mortgage"),
            _ => None,
        };
        let Some(write) = user_settings::put(home::SETTING_KEY, v) else {
            self.note = Some((false, "This server keeps no settings (it is older than the Home setup): update it first.".into()));
            cx.notify();
            return;
        };
        self.busy = true;
        self.note = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = json_of(write.await).map_err(|e| user_settings::last_error().unwrap_or(e));
            let mut rule = None;
            let outcome = match (saved, plan) {
                (Err(e), _) => Err(e),
                (Ok(_), None) => Ok("Saved.".to_string()),
                (Ok(_), Some((create, draft))) => {
                    let made = match create {
                        Some(body) => json_of(cats::create_category(body).await).map(|_| ()),
                        None => Ok(()),
                    };
                    match made.and(json_of(cats::create_rule(&draft).await)) {
                        Ok(reply) => {
                            rule = Some(reply);
                            Ok(format!("Saved, and created a rule: rows matching \u{201c}{}\u{201d} are {}.", draft.pattern, draft.category))
                        }
                        Err(e) => Err(format!("Saved, but the rule wasn't created: {e}")),
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.note = Some(match outcome {
                    Ok(t) => (true, t),
                    Err(e) => (false, e),
                });
                if let Some(r) = rule {
                    cx.emit(HomeSetupEvent::Written(r));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_pair(&mut self, overpayment: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (ph, list) = if overpayment { (("YYYY-MM-DD", "Amount"), 0) } else { (("YYYY-MM-DD", "Yearly rate, %"), 1) };
        let pair = self.new_pair(ph, (String::new(), String::new()), window, cx);
        if list == 0 { self.overs.push(pair) } else { self.changes.push(pair) }
        cx.notify();
    }

    fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // the first one is the deposit, the usual start
        let kinds = kinds_for(self.mode);
        let kind = kinds.iter().copied().find(|k| !self.oneoffs.iter().any(|r| r.kind == *k)).unwrap_or(kinds[0]);
        let row = self.new_row(None, kind, window, cx);
        self.oneoffs.push(row);
        cx.notify();
    }
}

impl Render for HomeSetupForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.filled && user_settings::server_side() {
            self.fill(window, cx);
        }
        // suggestions from the ledger: placeholders (once per suggestion), the
        // captions and the "Use suggestions" button
        let sug = self.suggestion();
        let id = Rc::as_ptr(&sug) as usize;
        if self.placeholders_for != id {
            self.placeholders_for = id;
            self.show_placeholders(&sug, window, cx);
        }
        let saved = super::spending_home::setup().is_some();
        let plan = self.plan(&sug, saved, cx);
        let hint = |text: Option<String>, cx: &App| text.map(|t| ui::caption_wrap(format!("suggested: {t}"), cx));
        let (h_currency, h_monthly, h_start, h_end, h_landlord, h_loan_start, h_lender) = (
            hint(plan.why(Target::Currency).map(String::from), cx), hint(plan.why(Target::Monthly).map(String::from), cx),
            hint(plan.why(Target::LeaseStart).map(String::from), cx), hint(plan.why(Target::LeaseEnd).map(String::from), cx),
            hint(plan.why(Target::Landlord).map(String::from), cx), hint(plan.why(Target::LoanStart).map(String::from), cx),
            hint(plan.why(Target::Lender).map(String::from), cx),
        );
        let h_loan: Vec<Div> = [Target::Principal, Target::Rate, Target::Term].into_iter()
            .filter_map(|t| plan.fills.iter().find(|f| f.target == t).map(|f| (t, f)))
            .filter_map(|(t, f)| hint(Some(format!("{} {}: {}", match t { Target::Principal => "amount borrowed", Target::Rate => "rate", _ => "term" }, f.value, f.why)), cx))
            .collect();
        let h_mode = (!saved).then(|| sug.mode.as_ref().map(|m| m.why.clone())).flatten()
            .and_then(|t| hint(Some(t), cx));
        let h_costs: Vec<Div> = plan.oneoffs.iter().filter_map(|o| hint(Some(format!("{} {} ({})", kind_label(&o.value.kind), number(o.value.amount), o.why)), cx)).collect();
        let use_button = (!plan.is_empty()).then(|| h_flex().gap_2().items_center().flex_wrap()
            .child(Button::new("home-suggest").small().outline().icon(IconName::Star).label("Use suggestions")
                .on_click(cx.listener(|this, _, w, cx| this.use_suggestions(w, cx))))
            .child(div().flex_1().min_w(px(200.)).child(ui::caption_wrap("fills the empty fields from your transactions; nothing is saved until Save", cx))));
        let t = cx.theme().clone();
        let field = |label: &'static str, s: &Entity<InputState>, w: f32| {
            v_flex().flex_1().min_w(crate::layout::col(w)).gap(px(2.)).child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(label)).child(Input::new(s).small())
        };
        let choice = |id: String, label: &'static str, on: bool, cx: &mut Context<Self>, f: Box<dyn Fn(&mut Self, &mut Context<Self>)>| {
            div().id(SharedString::from(id)).px(px(10.)).py(px(3.)).rounded(px(6.)).text_xs().cursor_pointer().whitespace_nowrap()
                .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| { f(this, cx); cx.notify(); }))
                .child(label)
                .into_any_element()
        };
        let hair = ui::hairline(cx);
        let segmented = |c: Vec<AnyElement>| h_flex().p(px(2.)).gap(px(2.)).rounded(px(7.)).border_1().border_color(hair).children(c);
        let rent = self.mode == "rent";
        let mortgage = self.mode == "mortgage";
        let pick = |mode: &'static str| -> Box<dyn Fn(&mut Self, &mut Context<Self>)> {
            Box::new(move |t, _| {
                t.mode = mode;
                // a cost's kind must be one the mode offers
                let kinds = kinds_for(mode);
                for r in t.oneoffs.iter_mut().filter(|r| !kinds.contains(&r.kind)) {
                    r.kind = kinds[0];
                }
            })
        };
        let mode = segmented(vec![
            choice("home-mode-rent".into(), "Rent", rent, cx, pick("rent")),
            choice("home-mode-mortgage".into(), "Mortgage", mortgage, cx, pick("mortgage")),
            choice("home-mode-none".into(), "None", self.mode == "none", cx, pick("none")),
        ]);
        let mut card = ui::card(cx).gap_2()
            .child(ui::field_row("Mode", h_flex().gap_2().flex_wrap().child(mode)
                .child(ui::caption(match self.mode {
                    "rent" => "pay rent for my home",
                    "mortgage" => "pay a mortgage on my home",
                    _ => "have no rent or mortgage to track (I own outright, or live free)",
                }, cx)).children(h_mode), cx))
            .child(ui::field_row("Currency", v_flex().gap_1().child(div().w(px(90.)).child(Input::new(&self.currency).small())).children(h_currency), cx));
        if rent {
            let landlord = Self::text(&self.landlord, cx);
            card = card
                .child(ui::field_row("Monthly rent", v_flex().gap_1().child(div().max_w(px(220.)).child(Input::new(&self.monthly).small())).children(h_monthly), cx))
                .child(ui::field_row("Lease", v_flex().gap_1().child(h_flex().gap_2().flex_wrap().child(field("Start", &self.lease_start, 130.)).child(field("End", &self.lease_end, 130.)))
                    .children(h_start).children(h_end), cx))
                .child(ui::field_row("Landlord", v_flex().gap_1().child(Input::new(&self.landlord).small()).children(h_landlord)
                    .child(ui::caption_wrap(if landlord.is_empty() {
                        "Optional. Creates a rule: money-out rows matching this text are Rent.".to_string()
                    } else {
                        format!("Creates a rule: money-out rows matching \u{201c}{landlord}\u{201d} are Rent.")
                    }, cx)), cx));
        }
        if mortgage {
            let lender = Self::text(&self.lender, cx);
            let hair2 = hair;
            let list = |title: &'static str, note: &'static str, items: &[Pair], what: (&'static str, &'static str), which: u8, cx: &mut Context<Self>| {
                let rows: Vec<Div> = items.iter().enumerate().map(|(i, p)| {
                    h_flex().gap_2().flex_wrap().items_end()
                        .child(field(what.0, &p.when, 130.)).child(field(what.1, &p.value, 100.))
                        .child(Button::new(SharedString::from(format!("home-m{which}-rm-{i}"))).xsmall().ghost().icon(IconName::Close).tooltip("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let l = if which == 0 { &mut this.overs } else { &mut this.changes };
                                if i < l.len() { l.remove(i); }
                                cx.notify();
                            })))
                }).collect();
                v_flex().gap_1().pt_2().border_t_1().border_color(hair2)
                    .child(ui::eyebrow(title, cx)).child(ui::caption_wrap(note, cx)).children(rows)
                    .child(h_flex().child(Button::new(SharedString::from(format!("home-m{which}-add"))).xsmall().outline().icon(IconName::Plus).label("Add")
                        .on_click(cx.listener(move |this, _, w, cx| this.add_pair(which == 0, w, cx)))))
            };
            card = card
                .child(ui::field_row("Loan", v_flex().gap_1().child(h_flex().gap_2().flex_wrap()
                    .child(field("Borrowed", &self.principal, 110.)).child(field("Rate % a year", &self.rate, 90.)).child(field("Term, months", &self.term, 90.)))
                    .children(h_loan), cx))
                .child(ui::field_row("Payments", v_flex().gap_1().child(h_flex().gap_2().flex_wrap()
                    .child(field("Loan start", &self.start, 130.)).child(field("Payment day", &self.pay_day, 90.))).children(h_loan_start), cx))
                .child(ui::field_row("Home value", h_flex().gap_2().flex_wrap()
                    .child(field("Value", &self.home_value, 110.)).child(field("As of", &self.home_value_date, 130.)), cx))
                .child(ui::field_row("Lender", v_flex().gap_1().child(Input::new(&self.lender).small()).children(h_lender)
                    .child(ui::caption_wrap(if lender.is_empty() {
                        "Optional. Creates a rule: money-out rows matching this text are Mortgage.".to_string()
                    } else {
                        format!("Creates a rule: money-out rows matching \u{201c}{lender}\u{201d} are Mortgage.")
                    }, cx)), cx))
                .child(list("Overpayments", "Extra principal you paid: it shortens the term and keeps the payment.", &self.overs, ("Date", "Amount"), 0, cx))
                .child(list("Rate changes", "From that date the new yearly rate applies and the payment is recomputed for the rest.", &self.changes, ("From", "Rate % a year"), 1, cx));
        }
        // the one-off costs
        let rows: Vec<Div> = self.oneoffs.iter().enumerate().map(|(i, r)| {
            let kinds = kinds_for(self.mode).iter().copied().map(|k| {
                choice(format!("home-kind-{i}-{k}"), kind_label(k), r.kind == k, cx, Box::new(move |t, _| if let Some(r) = t.oneoffs.get_mut(i) { r.kind = k }))
            });
            v_flex().gap_1().py_2().border_t_1().border_color(hair)
                .child(h_flex().gap_2().flex_wrap().items_center().justify_between()
                    .child(h_flex().p(px(2.)).gap(px(2.)).rounded(px(7.)).border_1().border_color(hair).children(kinds))
                    .child(Button::new(SharedString::from(format!("home-rm-{i}"))).xsmall().ghost().icon(IconName::Close).tooltip("Remove this cost")
                        .on_click(cx.listener(move |this, _, _, cx| { if i < this.oneoffs.len() { this.oneoffs.remove(i); } cx.notify(); }))))
                .child(h_flex().gap_2().flex_wrap().child(field("Amount", &r.amount, 90.)).child(field("Paid on", &r.date, 130.)).when(r.kind == "deposit", |d| d.child(field("Returned on", &r.returned, 130.))))
        }).collect();
        card = card
            .child(div().pt_2().child(ui::eyebrow(if mortgage { "What buying cost" } else { "What moving in cost" }, cx)))
            .when(!mortgage, |d| d.child(ui::caption_wrap("A deposit's returned date also closes it under Owed to you. Leave it empty while you still hold it.", cx)))
            .children(rows)
            .when(!h_costs.is_empty(), |d| d.child(v_flex().gap_1().child(ui::caption("From your Deposit, Agency fee and Stamp duty rows:", cx)).children(h_costs)))
            .child(h_flex().child(Button::new("home-add").small().outline().icon(IconName::Plus).label("Add a cost")
                .on_click(cx.listener(|this, _, w, cx| this.add_row(w, cx)))));
        let note = self.note.clone().map(|(ok, text)| div().text_xs().text_color(if ok { fmt::money_in() } else { fmt::money_out() }).child(text));
        card = card.child(h_flex().gap_2().flex_wrap().items_center().pt_2()
            .child(Button::new("home-save").small().primary().label("Save").loading(self.busy).disabled(self.busy)
                .on_click(cx.listener(|this, _, _, cx| this.save(cx))))
            .children(note));
        v_flex().gap_2()
            .child(ui::section("Home", "rent or mortgage, and what moving in cost", cx))
            .children(use_button)
            .child(card)
    }
}

#[cfg(test)]
mod tests {
    use super::{kind_label, kind_of, ONEOFF_KINDS};

    #[test]
    fn kinds_have_labels() {
        assert_eq!(ONEOFF_KINDS.map(kind_label), ["Deposit", "Agency fee", "Stamp duty", "Legal fee"]);
        assert_eq!(kind_of("stamp_duty"), "stamp_duty");
        assert_eq!(kind_of("bribe"), "deposit");
    }
}
