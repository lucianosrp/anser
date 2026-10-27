//! Deep dive › Dividends: income per holding and the tax withheld, from the
//! IBKR income rows (ibkr_income, carried by the ledger, so the web build
//! has them too); or, while there are none, what data is needed.

use finance_core::exposure;
use finance_core::invest;
use gpui_kit::component::{ActiveTheme as _, IconName, h_flex, v_flex};
use gpui_kit::*;

use super::{c, col, flex, note, table, takeaway, tone, Data};
use crate::fmt;
use crate::ui::{self, sp};
use crate::zoom::px;

pub fn render(d: &Data, cx: &App) -> Div {
    let t = cx.theme();
    let income = invest::income(&d.extra.cash_flows);
    // what pays: the held funds (the static table) and stocks, and their domicile
    let positions = super::exposure::broker_positions(&d.bundle);
    let payers: Vec<String> = d.held().iter().filter_map(|h| {
        let desc = positions.iter().find(|p| p.symbol == h.symbol).map(|p| p.description.as_str()).unwrap_or("");
        Some(format!("{} ({})", h.symbol, exposure::domicile_of(&h.symbol, desc, &h.currency)?))
    }).collect();
    if income.is_empty() {
        let kinds = invest::flow_kinds(&d.extra.cash_flows);
        let have = if kinds.is_empty() {
            "There are no IBKR cash transactions in the data at all.".to_string()
        } else {
            format!("The IBKR cash transactions in the data are only: {}.", kinds.iter().map(|(k, n)| format!("{k} ({n})")).collect::<Vec<_>>().join(", "))
        };
        return v_flex().gap(px(sp::GRID))
            .child(takeaway("No dividends or withholding tax in the data yet, so there is no income to show.", cx))
            .child(ui::card(cx).child(ui::empty_with(IconName::Inbox, "No dividend rows yet",
                &format!("{have} Dividends show up here once the IBKR Flex query's Cash Transactions section includes the Dividends, Payment In Lieu of Dividends and Withholding Tax types (they land in ibkr_income on the next sync; the Doctor's IBKR row says whether the query has them). Accumulating funds never pay out; distributing funds and US-listed stocks and ETFs do, usually quarterly."), cx)))
            .child(note(format!("Held: {}. As a general rule, US-domiciled funds and stocks have 30% withheld from dividends paid to a holder without a US tax treaty, while Irish-domiciled UCITS funds pay out without withholding and bear about 15% on US dividends inside the fund. Check your own tax position; the data will show the actual rates.", payers.join(", ")), cx));
    }

    let gross: f64 = income.iter().map(|i| i.gross_hkd).sum();
    let withheld: f64 = income.iter().map(|i| i.withheld_hkd).sum();
    let line = format!("{} in dividends so far, {} withheld ({:.0}%), {} net.",
        fmt::hkd(gross), fmt::hkd(withheld), if gross > 0. { withheld / gross * 100. } else { 0. }, fmt::hkd(gross - withheld));
    let rows: Vec<Vec<super::Cell>> = income.iter().map(|i| vec![
        c(i.symbol.clone()),
        c(fmt::count(i.payments as f64)),
        c(super::ccy(i.gross, &i.currency)),
        c(fmt::int(i.gross_hkd)),
        c(fmt::int(i.withheld_hkd)),
        tone(format!("{:.0}%", i.rate_pct()), if i.rate_pct() >= 25. { t.warning } else { t.foreground }),
        c(fmt::int(i.net_hkd())),
        c(i.last.clone()),
    ]).collect();
    let cols = [flex("Holding"), col("Payments", 70.), col("Gross", 100.), col(fmt::local_static("Gross HK$"), 80.), col(fmt::local_static("Withheld HK$"), 96.), col("Rate", 50.), col(fmt::local_static("Net HK$"), 72.), col("Last", 84.)];
    v_flex().gap(px(sp::GRID))
        .child(takeaway(line, cx))
        .child(h_flex().gap(px(sp::GRID)).flex_wrap().children([
            ui::stat("Dividends", fmt::hkd(gross), "gross, HKD at the payment's rate", None, cx),
            ui::stat("Withheld", fmt::hkd(withheld), "tax taken at source", Some(fmt::money_out()), cx),
            ui::stat("Net", fmt::hkd(gross - withheld), "what reached the account", Some(fmt::money_in()), cx),
        ].map(|x| x.min_w(px(180.)))))
        .child(ui::card(cx).gap_2()
            .child(ui::section("Per holding", "from the IBKR Flex cash transactions", cx).mt_0())
            .child(table("div-table", &cols, rows, cx)))
}
