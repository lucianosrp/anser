//! Deep dive › Targets: the owner's own target weights (targets.toml), each
//! holding's drift from them, and the rebalancing math for the next
//! contribution: how to split it so the broker ends as close to the targets
//! as possible without selling. Arithmetic, not a recommendation.

use std::collections::BTreeMap;

use finance_core::exposure::{venue_of, Kind};
use finance_core::fees::{self, Plan};
use finance_core::rebalance;
use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, button::{Button, ButtonVariants as _}, h_flex,
    input::{Input, InputEvent, InputState}, v_flex, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{c, col, exposure::broker_positions, flex, note, table, takeaway, tone, Data, DeepDive};
use crate::budgets::parse_amount;
use crate::fmt;
use crate::targets::Targets;
use crate::ui::{self, sp, ts};
use crate::zoom::px;

impl DeepDive {
    /// The contribution the split uses: the field, else the saved one, else
    /// the median deposit into the broker.
    fn contribution_hkd(&self, d: &Data, cx: &App) -> f64 {
        if let Some(v) = self.contribution.as_ref().and_then(|i| parse_amount(&i.read(cx).value())) {
            return v;
        }
        self.targets.contribution.unwrap_or_else(|| default_contribution(d))
    }

    fn ensure_contribution(&mut self, d: &Data, window: &mut Window, cx: &mut Context<Self>) {
        if self.contribution.is_some() {
            return;
        }
        let v = self.targets.contribution.unwrap_or_else(|| default_contribution(d));
        // privacy mode: the field shows dots like every other amount
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(fmt::base_ccy()).masked(fmt::private()).default_value(format!("{v:.0}")));
        self._subs.push(cx.subscribe_in(&input, window, |_, _, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                cx.notify();
            }
        }));
        self.contribution = Some(input);
    }

    fn start_edit(&mut self, symbols: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.weight_inputs = symbols.into_iter().map(|s| {
            let v = self.targets.weights.get(&s).map(|w| format!("{w}")).unwrap_or_default();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("—").default_value(v));
            (s, input)
        }).collect();
        self.editing = true;
        self.targets_note = None;
        cx.notify();
    }

    fn save_targets(&mut self, cx: &mut Context<Self>) {
        let weights: BTreeMap<String, f64> = self.weight_inputs.iter()
            .filter_map(|(s, i)| Some((s.clone(), parse_amount(&i.read(cx).value())?)))
            .collect();
        let contribution = self.contribution.as_ref().and_then(|i| parse_amount(&i.read(cx).value())).or(self.targets.contribution);
        let t = Targets { contribution, weights };
        match t.save() {
            Ok(p) => {
                self.targets_note = Some(format!("Saved {} targets to {}.", t.weights.len(), p.display()));
                self.targets = t;
                self.editing = false;
                self.weight_inputs.clear();
            }
            Err(e) => self.targets_note = Some(format!("Could not save: {e:#}")),
        }
        cx.notify();
    }

    pub(super) fn render_targets(&mut self, d: &Data, window: &mut Window, cx: &mut Context<Self>) -> Div {
        self.ensure_contribution(d, window, cx);
        let t = cx.theme().clone();
        let values: Vec<(String, f64)> = broker_positions(&d.bundle).into_iter()
            .filter(|p| p.kind != Kind::Cash).map(|p| (p.symbol, p.value_hkd)).collect();
        let cash = d.bundle.f64(&["ibkr", "snapshot", "cash_base"]).unwrap_or(0.);
        let mut symbols: Vec<String> = values.iter().map(|v| v.0.clone()).collect();
        for k in self.targets.weights.keys() {
            if !symbols.contains(k) {
                symbols.push(k.clone());
            }
        }
        let contribution = self.contribution_hkd(d, cx);
        let has_targets = !rebalance::normalize(&self.targets.weights).is_empty();

        // ---- the editor / the controls ----
        let controls = if self.editing {
            let fields = h_flex().gap_3().flex_wrap().children(self.weight_inputs.iter().map(|(s, i)| {
                v_flex().gap(px(2.)).w(px(110.))
                    .child(ui::eyebrow(s.clone(), cx))
                    .child(Input::new(i).small())
            }));
            ui::card(cx).gap_3()
                .child(h_flex().justify_between().flex_wrap().gap_2()
                    .child(div().text_size(px(ts::TITLE)).font_semibold().child("Target weights"))
                    .child(h_flex().gap_2()
                        .child(Button::new("t-cancel").small().ghost().label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| { this.editing = false; this.weight_inputs.clear(); cx.notify(); })))
                        .child(Button::new("t-save").small().primary().label("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_targets(cx))))))
                .child(fields)
                .child(note("Any numbers: they are scaled to add up to 100%. An empty field means no target (that holding gets no new money in the split). Saved on this machine only, in targets.toml.", cx))
        } else {
            let syms = symbols.clone();
            h_flex().gap_3().items_center().flex_wrap()
                .child(Button::new("t-edit").small().outline().label(if has_targets { "Edit targets" } else { "Set target weights" })
                    .on_click(cx.listener(move |this, _, w, cx| this.start_edit(syms.clone(), w, cx))))
                .child(h_flex().gap_2().items_center()
                    .child(ui::caption("Next contribution", cx))
                    .child(div().w(px(120.)).when_some(self.contribution.as_ref(), |d, i| d.child(Input::new(i).small().prefix(div().text_xs().text_color(t.muted_foreground).child(fmt::sym()))))))
                .children(self.targets_note.clone().map(|n| ui::caption(n, cx)))
        };

        if !has_targets {
            let total: f64 = values.iter().map(|v| v.1).sum();
            let rows: Vec<Vec<super::Cell>> = values.iter().map(|(s, v)| vec![
                c(s.clone()), c(fmt::int(*v)), c(format!("{:.1}%", if total > 0. { v / total * 100. } else { 0. })),
            ]).collect();
            return v_flex().gap(px(sp::GRID))
                .child(takeaway("No target weights yet. Set them to see each holding's drift and how a contribution would split without selling.", cx))
                .child(controls)
                .child(ui::card(cx).gap_2()
                    .child(ui::section("Weights now", "the broker's holdings at its marks, cash aside", cx).mt_0())
                    .child(table("tgt-now", &[flex("Holding"), col(fmt::local_static("Value HK$"), 90.), col("Weight", 64.)], rows, cx)));
        }

        let drift = rebalance::drift(&values, &self.targets.weights);
        let split = rebalance::split(&values, &self.targets.weights, contribution);
        let after: Vec<(String, f64)> = drift.iter().map(|r| (r.symbol.clone(), r.value + split.iter().find(|s| s.0 == r.symbol).map(|s| s.1).unwrap_or(0.))).collect();
        let drift_after = rebalance::drift(&after, &self.targets.weights);
        let worst = drift.iter().max_by(|a, b| a.drift_pp.abs().total_cmp(&b.drift_pp.abs()));
        let worst_after = drift_after.iter().map(|r| r.drift_pp.abs()).fold(0., f64::max);
        let parts: Vec<String> = split.iter().filter(|s| s.1 >= 1.).map(|s| format!("{} {}", s.0, fmt::hkd(s.1))).collect();
        let line = format!("{}Rebalancing math: {} split without selling as {}; the largest drift goes from {:.1} to {:.1} points.",
            worst.map(|w| format!("Furthest from target: {} at {:+.1} points. ", w.symbol, w.drift_pp)).unwrap_or_default(),
            fmt::hkd(contribution), if parts.is_empty() { "nothing".to_string() } else { parts.join(", ") },
            worst.map(|w| w.drift_pp.abs()).unwrap_or(0.), worst_after);

        let warn = t.warning;
        let drift_rows: Vec<Vec<super::Cell>> = drift.iter().map(|r| vec![
            c(r.symbol.clone()),
            c(fmt::int(r.value)),
            c(format!("{:.1}%", r.weight)),
            c(if r.target > 0. { format!("{:.1}%", r.target) } else { "—".into() }),
            tone(fmt::signed(r.drift_pp, 1), if r.drift_pp.abs() >= 5. { warn } else { t.foreground }),
            tone(fmt::signed_hkd(r.gap).replace(&fmt::sym_space(), ""), if r.gap >= 0. { t.foreground } else { t.muted_foreground }),
        ]).collect();
        let drift_cols = [flex("Holding"), col(fmt::local_static("Value HK$"), 90.), col("Weight", 64.), col("Target", 64.), col("Drift pp", 70.), col(fmt::local_static("To target HK$"), 104.)];

        // each part as an order: its size in the trading currency and its Fixed commission
        let split_rows: Vec<Vec<super::Cell>> = split.iter().map(|(s, hkd)| {
            let plan = d.plans.iter().find(|h| &h.symbol == s);
            let order = plan.filter(|_| *hkd >= 1.).and_then(|h| {
                let amount = hkd / h.fx_now;
                let sch = fees::schedule(Plan::Fixed, venue_of(&h.symbol, &h.currency), &h.currency)?;
                let (fee, bind) = fees::commission(sch, amount / h.dca.price, h.dca.price);
                Some((amount, h.currency.clone(), fee, bind))
            });
            let w_after = drift_after.iter().find(|r| &r.symbol == s).map(|r| r.weight).unwrap_or(0.);
            vec![
                c(s.clone()),
                c(if *hkd >= 1. { fmt::int(*hkd) } else { "—".into() }),
                c(order.as_ref().map(|o| super::ccy(o.0, &o.1)).unwrap_or("—".into())),
                order.as_ref().map(|o| tone(format!("{} ({:.2}%)", super::ccy(o.2, &o.1), o.2 / o.0 * 100.), if o.3.is_floor() && o.2 / o.0 * 100. > 0.25 { warn } else { t.foreground })).unwrap_or(c("—")),
                c(format!("{w_after:.1}%")),
            ]
        }).collect();
        let split_cols = [flex("Holding"), col(fmt::local_static("Buy HK$"), 80.), col("Order", 110.), col("Fixed fee", 136.), col("Weight after", 90.)];

        v_flex().gap(px(sp::GRID))
            .child(takeaway(line, cx))
            .child(controls)
            .child(v_flex().gap(px(sp::GRID))
                .child(div().child(ui::card(cx).gap_2()
                    .child(ui::section_info("Drift", "the broker's holdings against your targets",
                        "Weight: share of the broker's holdings (cash aside) at the broker's marks. Target: your weights scaled to 100%. Drift: weight minus target, in percentage points (amber from 5). To target: what it would take to sit exactly at the target today.", cx).mt_0())
                    .child(table("tgt-drift", &drift_cols, drift_rows, cx))))
                .child(div().child(ui::card(cx).gap_2()
                    .child(ui::section_info("Rebalancing math", &format!("{} without selling", fmt::hkd(contribution)),
                        "The split that leaves the holdings as close to the targets as the money allows, without selling: holdings under target get what they lack, the most underweight first. Order: the part in the trading currency at the last close. Fixed fee: what that order would pay on the account's plan (amber when the minimum makes it more than 0.25%).", cx).mt_0())
                    .child(table("tgt-split", &split_cols, split_rows, cx)))))
            .child(note(format!("Rebalancing math on your own target weights, not a recommendation.{}",
                if cash > 1. { format!(" The account also holds {} of uninvested cash.", fmt::hkd(cash)) } else { String::new() }), cx))
    }
}

/// The median deposit into the broker, rounded to 1,000 (10,000 without any).
fn default_contribution(d: &Data) -> f64 {
    let mut deps: Vec<f64> = d.extra.cash_flows.iter().filter(|f| f.amount_base > 0. && f.kind.to_lowercase().contains("deposit")).map(|f| f.amount_base).collect();
    if deps.is_empty() {
        return 10_000.;
    }
    deps.sort_by(f64::total_cmp);
    ((deps[deps.len() / 2] / 1000.).round() * 1000.).max(1000.)
}
