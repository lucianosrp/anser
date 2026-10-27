//! Settings → Investments (Phase 4): the investment setup the server values
//! the holdings with (engine invest_setup), on the desktop and in the
//! browser.
//!
//! * Accounts: where money sent from a bank goes (the description fragments
//!   that file an Investments row to it), how it is valued (the IBKR data,
//!   an index proxy and the fund's real units, or at cost) and the fund
//!   platform's fees. Add, edit (one at a time), remove.
//! * Instruments: the yearly fee (TER) per symbol; a single stock has none.
//! * The benchmark the Investments page compares against (the profile's).
//!
//! Reading is open; every change is an admin route (the server says so when
//! this device isn't an admin's).

use finance_core::client::Request;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{json, Value};

use super::sources::{call, json_of};
use finance_core::categories::encode;
use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

/// The setup changed on the server (the app re-syncs: values may move).
pub struct InvestChanged;

/// The account form's fields: (key, label, width).
const FIELDS: [(&str, &str, f32); 10] = [
    ("name", "Name", 180.),
    ("matches", "Bank text (comma-separated)", 220.),
    ("proxy", "Index proxy", 90.),
    ("proxy_label", "It tracks", 120.),
    ("fund_code", "Fund code", 90.),
    ("fund_ocf_pct", "Fund fee %/yr", 90.),
    ("platform_flat_month", "Platform /month", 100.),
    ("platform_cliff", "Flat up to", 100.),
    ("platform_pct_above", "Then %/yr", 80.),
    ("fee_match", "Fee rows' text", 130.),
];

pub struct InvestSetup {
    setup: Option<Value>,
    load_error: Option<String>,
    /// the account being edited: Some("") a new one
    editing: Option<String>,
    valued_by: String,
    inputs: Vec<(&'static str, Entity<InputState>)>,
    sym: Entity<InputState>,
    ter: Entity<InputState>,
    stock: bool,
    bench: Entity<InputState>,
    fill_bench: bool,
    busy: bool,
    note: Option<(bool, String)>,
}

impl EventEmitter<InvestChanged> for InvestSetup {}

fn num_text(v: &Value) -> String {
    match v.as_f64() {
        Some(x) if x.fract() == 0.0 => format!("{x:.0}"),
        Some(x) => format!("{x}"),
        None => String::new(),
    }
}

impl InvestSetup {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        let mut v = InvestSetup {
            setup: None,
            load_error: None,
            editing: None,
            valued_by: "cost".into(),
            inputs: Vec::new(),
            sym: input("Symbol", window, cx),
            ter: input("TER %", window, cx),
            stock: false,
            bench: input("^GSPC", window, cx),
            fill_bench: false,
            busy: false,
            note: None,
        };
        v.refresh(cx);
        v
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if finance_core::signin::needed() {
            return;
        }
        call(Request::new("GET", "/api/investments/setup"), cx, |this, r, _| match json_of(r) {
            Ok(v) => {
                this.setup = Some(v);
                this.fill_bench = true;
            }
            Err(e) => this.load_error = Some(e),
        });
    }

    fn accounts(&self) -> Vec<Value> {
        self.setup.as_ref().and_then(|s| s["accounts"].as_array().cloned()).unwrap_or_default()
    }

    fn edit(&mut self, name: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let a = name.and_then(|n| self.accounts().into_iter().find(|a| a["name"] == json!(n))).unwrap_or(Value::Null);
        self.valued_by = a["valued_by"].as_str().unwrap_or("cost").to_string();
        self.inputs = FIELDS
            .iter()
            .map(|(k, label, _)| {
                let v = match *k {
                    "matches" => a["matches"].as_array().map(|m| m.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                    "fund_ocf_pct" | "platform_flat_month" | "platform_cliff" | "platform_pct_above" => num_text(&a[*k]),
                    _ => a[*k].as_str().unwrap_or("").to_string(),
                };
                let label = label.to_string();
                (*k, cx.new(|cx| InputState::new(window, cx).placeholder(label).default_value(v)))
            })
            .collect();
        self.editing = Some(name.unwrap_or("").to_string());
        self.note = None;
        cx.notify();
    }

    fn form_value(&self, cx: &App) -> Value {
        let get = |k: &str| self.inputs.iter().find(|(x, _)| *x == k).map(|(_, i)| i.read(cx).value().trim().to_string()).unwrap_or_default();
        let opt = |k: &str| {
            let v = get(k);
            if v.is_empty() { Value::Null } else { json!(v) }
        };
        let numv = |k: &str| get(k).replace(',', "").parse::<f64>().ok().map(|x| json!(x)).unwrap_or(Value::Null);
        let matches: Vec<String> = get("matches").split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
        let sort = self.accounts().iter().position(|a| a["name"] == json!(self.editing.clone().unwrap_or_default()))
            .and_then(|i| self.accounts()[i]["sort"].as_i64())
            .unwrap_or_else(|| self.accounts().iter().filter_map(|a| a["sort"].as_i64()).max().unwrap_or(0) + 10);
        json!({
            "name": get("name"), "matches": matches, "sort": sort, "valued_by": self.valued_by,
            "proxy": opt("proxy"), "proxy_label": opt("proxy_label"), "fund_code": opt("fund_code"),
            "fund_ocf_pct": numv("fund_ocf_pct"), "platform_flat_month": numv("platform_flat_month"),
            "platform_cliff": numv("platform_cliff"), "platform_pct_above": numv("platform_pct_above"), "fee_match": opt("fee_match"),
        })
    }

    /// Send a write; on success take the setup from the reply.
    fn send(&mut self, what: &str, req: Request, cx: &mut Context<Self>) {
        self.busy = true;
        let what = what.to_string();
        call(req, cx, move |this, r, cx| {
            this.busy = false;
            match json_of(r) {
                Ok(v) => {
                    this.setup = Some(v);
                    this.editing = None;
                    this.inputs.clear();
                    this.note = Some((true, format!("{what}. The values follow on the next sync.")));
                    cx.emit(InvestChanged);
                }
                Err(e) => this.note = Some((false, e)),
            }
        });
        cx.notify();
    }

    fn save_account(&mut self, cx: &mut Context<Self>) {
        let Some(old) = self.editing.clone() else { return };
        let body = self.form_value(cx);
        let req = if old.is_empty() {
            Request::new("POST", "/api/admin/investments/accounts")
        } else {
            Request::new("PUT", &format!("/api/admin/investments/accounts/{}", encode(&old)))
        };
        self.send("Saved", req.json(&body), cx);
    }

    fn save_instrument(&mut self, cx: &mut Context<Self>) {
        let sym = self.sym.read(cx).value().trim().to_uppercase();
        if sym.is_empty() {
            self.note = Some((false, "A symbol first, like VWRA".into()));
            cx.notify();
            return;
        }
        let ter = self.ter.read(cx).value().trim().trim_end_matches('%').parse::<f64>().ok();
        let body = json!({"kind": if self.stock { "stock" } else { "fund" }, "ter_pct": if self.stock { Some(0.0) } else { ter }});
        self.send(&format!("{sym} saved"), Request::new("PUT", &format!("/api/admin/investments/instruments/{}", encode(&sym))).json(&body), cx);
    }

    fn save_benchmark(&mut self, cx: &mut Context<Self>) {
        let b = self.bench.read(cx).value().trim().to_string();
        self.busy = true;
        // the profile is written whole: read it, change the benchmark
        call(Request::new("GET", "/api/profile"), cx, move |this, r, cx| match json_of(r) {
            Ok(mut p) => {
                p["benchmark"] = if b.is_empty() { Value::Null } else { json!(b) };
                call(Request::new("PUT", "/api/profile").json(&p), cx, |this, r, cx| {
                    this.busy = false;
                    match json_of(r) {
                        Ok(_) => {
                            this.note = Some((true, "Benchmark saved. The Investments page follows on the next sync.".into()));
                            this.refresh(cx);
                            cx.emit(InvestChanged);
                        }
                        Err(e) => this.note = Some((false, e)),
                    }
                });
            }
            Err(e) => {
                this.busy = false;
                this.note = Some((false, e));
            }
        });
        cx.notify();
    }

    fn account_line(a: &Value) -> String {
        let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
        let mut parts = Vec::new();
        let m: Vec<&str> = a["matches"].as_array().map(|m| m.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if !m.is_empty() {
            parts.push(format!("bank text: {}", m.iter().map(|x| format!("\"{}\"", x.trim())).collect::<Vec<_>>().join(", ")));
        }
        parts.push(match s("valued_by").as_str() {
            "ibkr" => "valued by the IBKR data".to_string(),
            "proxy" => {
                let mut v = format!("valued by {}", s("proxy"));
                if !s("proxy_label").is_empty() {
                    v.push_str(&format!(" ({})", s("proxy_label")));
                }
                if !s("fund_code").is_empty() {
                    v.push_str(&format!(", fund {}", s("fund_code")));
                }
                v
            }
            _ => "at cost".to_string(),
        });
        if let Some(o) = a["fund_ocf_pct"].as_f64() {
            parts.push(format!("fund {o}%/yr"));
        }
        if let Some(f) = a["platform_flat_month"].as_f64() {
            let cliff = a["platform_cliff"].as_f64().map(|c| format!(" up to {}", fmt::count(c))).unwrap_or_default();
            let above = a["platform_pct_above"].as_f64().map(|p| format!(", then {p}%/yr")).unwrap_or_default();
            parts.push(format!("platform {} {}/month{cliff}{above}", fmt::sym(), num_text(&json!(f))));
        }
        parts.join(" · ")
    }

    fn form(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let valued = self.valued_by.clone();
        let view = cx.entity().downgrade();
        let vb = Button::new("inv-valued").xsmall().outline()
            .label(match valued.as_str() { "ibkr" => "The IBKR data", "proxy" => "An index proxy", _ => "At cost" })
            .dropdown_menu(move |mut menu, _, _| {
                for (k, l) in [("ibkr", "The IBKR data (broker marks)"), ("proxy", "An index proxy (and the fund's real units)"), ("cost", "At cost")] {
                    let view = view.clone();
                    menu = menu.item(PopupMenuItem::new(l).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.valued_by = k.into();
                            cx.notify();
                        });
                    }));
                }
                menu
            });
        let field = |k: &str, label: &str, w: f32, i: &Entity<InputState>, cx: &App| {
            let _ = k;
            v_flex().gap(px(3.)).child(div().text_size(px(ts::CAPTION)).text_color(cx.theme().muted_foreground).child(label.to_string()))
                .child(div().w(px(w)).child(Input::new(i).small()))
        };
        let shown: Vec<AnyElement> = self.inputs.iter().filter_map(|(k, i)| {
            let (_, label, w) = FIELDS.iter().find(|(x, _, _)| x == k)?;
            let proxy_only = matches!(*k, "proxy" | "proxy_label" | "fund_code");
            (!proxy_only || valued == "proxy").then(|| field(k, label, *w, i, cx).into_any_element())
        }).collect();
        v_flex().gap_2().p_2().rounded(t.radius).border_1().border_color(ui::hairline(cx))
            .child(h_flex().gap_2().items_center()
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child("Valued by"))
                .child(vb))
            .child(h_flex().gap_3().flex_wrap().items_end().children(shown))
            .child(h_flex().gap_2()
                .child(Button::new("inv-save").small().primary().label("Save").loading(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.save_account(cx))))
                .child(Button::new("inv-cancel").small().ghost().label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.editing = None;
                        this.inputs.clear();
                        cx.notify();
                    }))))
    }
}

impl Render for InvestSetup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let Some(setup) = self.setup.clone() else {
            return ui::card(cx).child(div().text_sm().text_color(t.muted_foreground).child(match &self.load_error {
                Some(e) => format!("Couldn't load the investment setup: {e}"),
                None => "Loading the investment setup…".into(),
            }));
        };
        if std::mem::take(&mut self.fill_bench) {
            let b = setup["benchmark"].as_str().unwrap_or("").to_string();
            self.bench.update(cx, |i, cx| i.set_value(b, window, cx));
        }
        let caption = |s: String| div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(s);
        let mut card = ui::card(cx).gap_2();
        for (i, a) in setup["accounts"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let name = a["name"].as_str().unwrap_or("").to_string();
            let editing = self.editing.as_deref() == Some(name.as_str());
            let (n1, n2) = (name.clone(), name.clone());
            card = card.child(v_flex().gap(px(2.))
                .child(h_flex().gap_2().items_center()
                    .child(div().text_sm().font_semibold().flex_1().child(name.clone()))
                    .when(!editing, |d| d.child(Button::new(("inv-edit", i)).xsmall().outline().icon(IconName::Settings).label("Edit")
                        .on_click(cx.listener(move |this, _, window, cx| this.edit(Some(&n1), window, cx)))))
                    .when(!editing, |d| d.child(Button::new(("inv-rm", i)).xsmall().ghost().label("Remove")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let req = Request::new("DELETE", &format!("/api/admin/investments/accounts/{}", encode(&n2)));
                            this.send(&format!("Removed {n2}"), req, cx);
                        })))))
                .child(caption(Self::account_line(a)))
                .when(editing, |d| d.child(self.form(cx))));
        }
        let adding = self.editing.as_deref() == Some("");
        card = card.when(adding, |d| d.child(self.form(cx)))
            .when(!adding, |d| d.child(h_flex().child(Button::new("inv-add").small().outline().icon(IconName::Plus).label("Add an account")
                .on_click(cx.listener(|this, _, window, cx| this.edit(None, window, cx))))))
            .child(caption("An Investments bank row goes to the first account whose bank text it contains; none: Other.".into()));

        // instruments
        let mut fees = ui::card(cx).gap_2();
        let list: Vec<Value> = setup["instruments"].as_array().cloned().unwrap_or_default();
        let rows: Vec<AnyElement> = list.iter().enumerate().map(|(i, x)| {
            let sym = x["symbol"].as_str().unwrap_or("").to_string();
            let what = if x["kind"] == "stock" { "single stock, no fee".to_string() } else {
                x["ter_pct"].as_f64().map(|p| format!("{p}% a year")).unwrap_or_else(|| "fee unknown".into())
            };
            let s2 = sym.clone();
            h_flex().gap_2().items_center()
                .child(div().w(px(70.)).text_sm().font_semibold().child(sym.clone()))
                .child(div().flex_1().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(what))
                .child(Button::new(("ins-rm", i)).xsmall().ghost().label("Remove")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let req = Request::new("DELETE", &format!("/api/admin/investments/instruments/{}", encode(&s2)));
                        this.send(&format!("Removed {s2}"), req, cx);
                    })))
                .into_any_element()
        }).collect();
        let stock = self.stock;
        fees = fees.children(rows)
            .child(h_flex().gap_2().flex_wrap().items_center()
                .child(div().w(px(90.)).child(Input::new(&self.sym).small()))
                .child(div().w(px(80.)).child(Input::new(&self.ter).small()))
                .child(Button::new("ins-stock").xsmall().ghost().label(if stock { "Single stock" } else { "Fund" })
                    .tooltip("A single stock has no holding fee")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stock = !this.stock;
                        cx.notify();
                    })))
                .child(Button::new("ins-save").small().outline().icon(IconName::Plus).label("Add or change")
                    .on_click(cx.listener(|this, _, _, cx| this.save_instrument(cx)))))
            .child(caption("The yearly fee of each fund held (TER), from the issuer's page. A symbol missing here shows as fee unknown in Costs.".into()));

        // benchmark
        let default = setup["default_benchmark"].as_str().unwrap_or("^GSPC").to_string();
        let bench = ui::card(cx).gap_2()
            .child(h_flex().gap_2().items_center()
                .child(div().w(px(140.)).child(Input::new(&self.bench).small()))
                .child(Button::new("bench-save").small().outline().label("Save").on_click(cx.listener(|this, _, _, cx| this.save_benchmark(cx)))))
            .child(caption(format!("A Yahoo symbol the Investments page compares your deposits against. Empty: {default} (the S&P 500).")));

        v_flex().gap_2()
            .child(card)
            .child(ui::section("Fund fees", "per symbol", cx))
            .child(fees)
            .child(ui::section("Benchmark", "the Investments page's yardstick", cx))
            .child(bench)
            .when_some(self.note.clone(), |d, (ok, n)| d.child(div().text_size(px(ts::CAPTION))
                .text_color(if ok { t.muted_foreground } else { fmt::money_out() }).child(n)))
    }
}
