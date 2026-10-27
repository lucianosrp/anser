//! Travel: the trips finance_core::travel discovers from where the money was
//! spent, newest first. A map on top shows every trip from home, or the
//! open trip's route. Each card shows when, where, how long and what it
//! cost (on the ground + booked ahead); a click expands its route, its
//! transport and its rows, and a row opens the detail sheet.

use std::cell::RefCell;
use std::rc::Rc;

use finance_core::travel::{self, Atlas, Mode, Route, Trip};
use finance_core::{Ledger, drill};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, chart::BarChart, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::map;
use crate::pages::spending::OnPick;
use crate::ui::{self, Figures as _, Pt};
use crate::zoom::px;

/// Expand or collapse trip `i` (index into the oldest-first list).
pub type OnToggle = Rc<dyn Fn(usize, &mut Window, &mut App)>;

pub const EXPLAIN: &str = "Trips are found, not tagged: card purchases abroad (the statement names the country and the foreign amount) and spending from foreign-currency accounts mark the days you were away from home. Away days a few days apart are one trip. Cost is everything spent abroad plus Travel-category bookings (flights, hotels, eSIMs) paid up to 4 months before. A EUR card used from Hong Kong, online services and transfers don't count. On the map, a stop is a city a receipt names (or the country's, snapped to the trip's nearest named city); rows that name nothing go where you were that day. Legs with a ticket or an airport receipt behind them are solid, the rest dashed. The map is drawn offline from bundled Natural Earth and GeoNames data.";

/// "2026-06-12" -> "12 Jun 2026"
fn day(d: &str) -> String {
    chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map(|d| d.format("%-d %b %Y").to_string()).unwrap_or_else(|_| d.to_string())
}

/// "12 Jun → 22 Jul 2026", "24 → 29 May 2026", "29 Jun 2025"
fn span(t: &Trip) -> String {
    let (a, b) = (day(&t.start), day(&t.end));
    if t.start == t.end {
        return a;
    }
    let (sa, sb): (Vec<&str>, Vec<&str>) = (a.split(' ').collect(), b.split(' ').collect());
    match (sa.as_slice(), sb.as_slice()) {
        ([d1, m1, y1], [_, m2, y2]) if y1 == y2 && m1 == m2 => format!("{d1} → {b}"),
        ([d1, m1, y1], [_, _, y2]) if y1 == y2 => format!("{d1} {m1} → {b}"),
        _ => format!("{a} → {b}"),
    }
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// "12 Jun", "24 → 29 May": a stop's days, year left out.
fn short_span(a: &str, b: &str) -> String {
    let f = |d: &str| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map(|d| d.format("%-d %b").to_string()).unwrap_or_else(|_| d.to_string());
    if a == b {
        return f(a);
    }
    let (fa, fb) = (f(a), f(b));
    match (fa.split_once(' '), fb.split_once(' ')) {
        (Some((d1, m1)), Some((_, m2))) if m1 == m2 => format!("{d1} → {fb}"),
        _ => format!("{fa} → {fb}"),
    }
}

/// The routes, worked out once per loaded ledger (the page re-renders on
/// every mouse move over the map).
fn atlas(trips: &[Trip], ledger: &Ledger) -> Rc<Atlas> {
    thread_local! {
        static CACHE: RefCell<Option<((usize, usize, usize), Rc<Atlas>)>> = const { RefCell::new(None) };
    }
    let key = (trips.as_ptr() as usize, trips.len(), ledger.entries.len());
    CACHE.with(|c| {
        if let Some((k, a)) = c.borrow().as_ref() {
            if *k == key {
                return a.clone();
            }
        }
        let a = Rc::new(travel::atlas(ledger, trips));
        *c.borrow_mut() = Some((key, a.clone()));
        a
    })
}

pub fn mode_icon(m: Mode) -> gpui_kit::assets::IconName {
    use gpui_kit::assets::IconName as I;
    match m {
        Mode::Flight => I::Plane,
        Mode::Train => I::TrainFront,
        Mode::Bus => I::BusFront,
        Mode::Boat => I::Ship,
        Mode::Ride => I::CarTaxiFront,
        Mode::Metro => I::TramFront,
        Mode::Stay => I::BedDouble,
        Mode::Booking => I::Ticket,
    }
}

/// "Bari, Italy"
fn place_line(p: &travel::Place) -> String {
    if p.cc == "EU" || travel::place_name(&p.cc) == p.name { p.name.clone() } else { format!("{}, {}", p.name, travel::place_name(&p.cc)) }
}

/// The map: every trip from home, or the open trip's route.
fn map_card(trips: &[Trip], atlas: &Atlas, open: Option<usize>, keep: &dyn Fn(usize) -> bool, title: &str, cx: &App) -> Div {
    let t = cx.theme();
    let mut tint: std::collections::BTreeMap<String, usize> = Default::default();
    let mut arcs = Vec::new();
    let mut dots = Vec::new();
    let (home, title, caption) = match open.and_then(|i| Some((trips.get(i)?, atlas.routes.get(i)?))) {
        Some((tr, r)) => {
            for (cc, d) in &r.country_days {
                *tint.entry(cc.clone()).or_default() += d;
            }
            for l in &r.legs {
                arcs.push(map::Arc { from: l.from.at, to: l.to.at, dashed: l.inferred, weight: 1. });
            }
            let max = r.stops.iter().map(|s| s.hkd).fold(1., f64::max);
            for s in &r.stops {
                dots.push(map::Dot {
                    at: s.place.at,
                    size: (s.hkd / max) as f32,
                    label: Some(s.place.name.clone()),
                    tip: vec![
                        place_line(&s.place),
                        format!("{} · {}", short_span(&s.first, &s.last), plural(s.rows.len(), "purchase")),
                        format!("{} spent{}", fmt::hkd(s.hkd), if s.inferred { " · place inferred" } else { "" }),
                    ],
                    inferred: s.inferred,
                });
            }
            (r.home.clone(), format!("{} · {}", tr.label(), span(tr)), "the route in date order · dashed legs are inferred · click the trip again for all trips")
        }
        None => {
            // one dot per place, however many trips went there
            let mut places: Vec<(travel::Place, f64, Vec<String>, bool)> = Vec::new();
            let mut seen_arcs: Vec<((i32, i32), (i32, i32))> = Vec::new();
            let key = |p: &travel::Place| ((p.at.lon * 10.) as i32, (p.at.lat * 10.) as i32);
            for (_, r) in atlas.routes.iter().enumerate().filter(|(i, _)| keep(*i)) {
                for (cc, d) in &r.country_days {
                    *tint.entry(cc.clone()).or_default() += d;
                }
                for l in &r.legs {
                    let k = (key(&l.from), key(&l.to));
                    if !seen_arcs.contains(&k) && !seen_arcs.contains(&(k.1, k.0)) {
                        seen_arcs.push(k);
                        arcs.push(map::Arc { from: l.from.at, to: l.to.at, dashed: l.inferred, weight: 0.25 });
                    }
                }
                for s in &r.stops {
                    let visit = format!("{} · {}", short_span(&s.first, &s.last), fmt::compact(s.hkd));
                    match places.iter_mut().find(|p| p.0 == s.place) {
                        Some(p) => {
                            p.1 += s.hkd;
                            p.2.push(visit);
                            p.3 &= s.inferred;
                        }
                        None => places.push((s.place.clone(), s.hkd, vec![visit], s.inferred)),
                    }
                }
            }
            let max = places.iter().map(|p| p.1).fold(1., f64::max);
            let mut by_spend: Vec<usize> = (0..places.len()).collect();
            by_spend.sort_by(|a, b| places[*b].1.total_cmp(&places[*a].1));
            for (rank, &i) in by_spend.iter().enumerate() {
                let (p, spent, visits, inferred) = &places[i];
                let mut tip = vec![place_line(p), format!("{} · {}", plural(visits.len(), "visit"), fmt::hkd(*spent))];
                tip.extend(visits.iter().rev().take(4).cloned());
                dots.push(map::Dot { at: p.at, size: (*spent / max) as f32, label: (rank < 8).then(|| p.name.clone()), tip, inferred: *inferred });
            }
            let home = atlas.routes.iter().enumerate().rev().find(|(i, _)| keep(*i)).or(atlas.routes.iter().enumerate().last())
                .map(|(_, r)| r.home.clone());
            match home {
                Some(h) => (h, title.to_string(), "every trip from home · dot size = spend · hover a stop"),
                None => return div(),
            }
        }
    };
    let max_days = tint.values().copied().max().unwrap_or(1).max(1) as f32;
    let m = map::Map {
        id: "travel-map".into(),
        height: 340.,
        home: Some((home.at, if home.cc == "EU" { "Home (Europe)".into() } else { format!("Home · {}", home.name) })),
        tint: tint.into_iter().map(|(cc, d)| (cc, d as f32 / max_days)).collect(),
        arcs,
        dots,
    };
    let legend = h_flex().gap_3().flex_wrap()
        .child(ui::legend(t.primary, "stop", "", cx))
        .child(ui::legend(t.primary.opacity(0.45), "more days, deeper tint", "", cx));
    ui::chart_card_with(&title, caption, legend, 340. + 64., map::render(m, cx), cx)
}

/// Inside an open trip card: the route, the way it was travelled, and the
/// transport and stays by mode.
fn transport(r: &Route, ledger: &Ledger, on_pick: OnPick, cx: &App) -> Div {
    let t = cx.theme();
    // Hong Kong ✈ Tokyo 🚆 Narita ✈ Hong Kong
    let mut path = h_flex().gap_2().flex_wrap().items_center().text_sm();
    if let Some(first) = r.legs.first() {
        path = path.child(div().font_semibold().child(first.from.name.clone()));
    }
    for l in &r.legs {
        let (icon, color) = match l.mode {
            Some(m) => (mode_icon(m), if l.inferred { t.muted_foreground.opacity(0.6) } else { t.primary }),
            None => (gpui_kit::assets::IconName::Ellipsis, t.muted_foreground.opacity(0.6)),
        };
        path = path.child(Icon::new(icon).xsmall().text_color(color)).child(div().child(l.to.name.clone()));
    }
    // flights 2 · HK$16.9k   trains …
    let chips = h_flex().gap_2().flex_wrap().children(r.by_mode.iter().map(|(m, n, v)| {
        ui::chip(format!("{} · {}", m.count(*n), fmt::compact(*v)), t.muted_foreground, cx)
            .bg(t.muted)
            .child(Icon::new(mode_icon(*m)).xsmall())
    }));
    let rows = v_flex().children(r.transit.iter().filter_map(|x| {
        let e = ledger.entries.get(x.row)?;
        let on_pick = on_pick.clone();
        let row = x.row;
        Some(h_flex()
            .id(("trip-transit", row))
            .gap_3()
            .py_1()
            .px_1()
            .rounded(t.radius)
            .cursor_pointer()
            .hover(|s| s.bg(t.accent))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                on_pick(row, window, cx)
            })
            .child(div().w(px(84.)).text_xs().text_color(t.muted_foreground).child(e.date.get(..10).unwrap_or(&e.date).to_string()))
            .child(div().w(px(18.)).flex().justify_center().child(Icon::new(mode_icon(x.mode)).small().text_color(t.muted_foreground)))
            .child(v_flex().flex_1().min_w_0()
                .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(e.description.clone()))
                .child(div().text_xs().text_color(t.muted_foreground).child(x.mode.label())))
            .child(div().w(px(104.)).text_sm().text_right().whitespace_nowrap().figures().text_color(fmt::money_out()).child(fmt::hkd(-x.hkd))))
    }));
    let mut out = v_flex().gap_2().child(path);
    if !r.by_mode.is_empty() {
        out = out.child(chips).child(rows);
    } else {
        out = out.child(ui::caption("No transport or stays among the rows: the legs are inferred from where the money was spent.", cx));
    }
    out
}

/// Pick a year (None = every year).
pub type OnYear = Rc<dyn Fn(Option<i32>, &mut Window, &mut App)>;

fn start_year(t: &Trip) -> Option<i32> {
    t.start.get(..4)?.parse().ok()
}

/// "All years · 2026 · 2025 · …" as a segmented control.
fn year_bar(trips: &[Trip], year: Option<i32>, on_year: OnYear, cx: &App) -> Div {
    let t = cx.theme();
    let mut years: Vec<i32> = trips.iter().filter_map(start_year).collect();
    years.sort_unstable_by(|a, b| b.cmp(a));
    years.dedup();
    let seg = |label: String, this: Option<i32>, k: usize| {
        let on = year == this;
        let on_year = on_year.clone();
        div().id(("trip-year", k)).px(px(10.)).py(px(3.)).rounded(px(6.)).text_xs().cursor_pointer().whitespace_nowrap()
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(move |_, w, cx| on_year(this, w, cx))
            .child(label)
    };
    h_flex().p(px(3.)).gap(px(2.)).flex_wrap().rounded(px(8.)).border_1().border_color(ui::hairline(cx))
        .child(seg("All years".into(), None, 0))
        .children(years.into_iter().enumerate().map(|(k, y)| {
            let n = trips.iter().filter(|t| start_year(t) == Some(y)).count();
            seg(format!("{y} · {n}"), Some(y), k + 1)
        }))
}

#[allow(clippy::too_many_arguments)]
pub fn render(trips: &[Trip], ledger: &Ledger, open: Option<usize>, year: Option<i32>, on_year: OnYear, on_toggle: OnToggle,
              on_pick: OnPick, on_search: crate::pages::spending::OnSearch, cx: &App) -> ui::Sections {
    let t = cx.theme();
    if trips.is_empty() {
        return ui::Sections::new().child(ui::empty_with(gpui_kit::component::IconName::Globe, "No trips found yet",
            "Trips appear once card or account data shows spending abroad.", cx));
    }
    let atlas = atlas(trips, ledger);
    let keep = |i: usize| year.is_none() || trips.get(i).and_then(start_year) == year;
    let open = open.filter(|&i| keep(i));

    // ---- the last 12 months, or the year picked ----
    let year_ago = (chrono::Utc::now().date_naive() - chrono::Duration::days(365)).format("%Y-%m-%d").to_string();
    let recent: Vec<&Trip> = match year {
        Some(_) => trips.iter().enumerate().filter(|(i, _)| keep(*i)).map(|(_, t)| t).collect(),
        None => trips.iter().filter(|t| t.end >= year_ago).collect(),
    };
    let period = year.map(|y| format!("in {y}")).unwrap_or_else(|| "last 12 months".into());
    let days: i64 = recent.iter().map(|t| t.days()).sum();
    let away: f64 = recent.iter().map(|t| t.away_hkd).sum();
    let booked: f64 = recent.iter().map(|t| t.booked_hkd).sum();
    let biggest = trips.iter().enumerate().filter(|(i, _)| keep(*i)).map(|(_, t)| t).max_by(|a, b| a.total_hkd().total_cmp(&b.total_hkd()));
    let cards = h_flex().gap_4().flex_wrap().items_stretch().children([
        ui::stat("Trips", recent.len().to_string(), format!("{} away · {period}", plural(days as usize, "day")), None, cx),
        ui::stat("Spent travelling", fmt::hkd(away + booked),
            format!("on the ground {} · booked {}", fmt::compact(away), fmt::compact(booked)), Some(fmt::money_out()), cx),
        ui::stat("Per day away", if days > 0 { fmt::hkd(away / days as f64) } else { "—".into() },
            "on the ground, bookings excluded", None, cx),
        ui::stat("Biggest trip", biggest.map(|b| fmt::hkd(b.total_hkd())).unwrap_or_default(),
            biggest.map(|b| format!("{} · {}", b.label(), span(b))).unwrap_or_default(), None, cx),
    ].map(|d| d.min_w(px(220.))));

    // ---- cost per trip, oldest first ----
    let mut seen = std::collections::HashMap::<String, usize>::new();
    let all_pts: Vec<Pt> = trips.iter().map(|tr| {
        let mut label = chrono::NaiveDate::parse_from_str(&tr.start, "%Y-%m-%d").map(|d| d.format("%b %y").to_string()).unwrap_or_default();
        // two trips starting in one month need distinct bands
        let n = seen.entry(label.clone()).or_default();
        *n += 1;
        if *n > 1 {
            label = format!("{label} ({n})");
        }
        Pt { label: label.into(), v: tr.total_hkd() }
    }).collect();
    let open_label = open.and_then(|i| all_pts.get(i)).map(|p| p.label.clone());
    let pts: Vec<Pt> = all_pts.into_iter().enumerate().filter(|(i, _)| keep(*i)).map(|(_, p)| p).collect();
    let shown = pts.len();
    let (on, off) = (t.primary, t.primary.opacity(if open_label.is_some() { 0.35 } else { 0.8 }));
    let chart = BarChart::new(pts).appear(false)
        .band(|p: &Pt| p.label.clone())
        .value(|p: &Pt| p.v)
        .value_axis(true)
        .tick_margin((shown / 12).max(1))
        .value_tick_format(fmt::compact)
        .tooltip_value(|_, v| fmt::hkd(v).into())
        .fill(move |p: &Pt, _, _, _| if Some(&p.label) == open_label.as_ref() { on } else { off });

    // ---- the trips, newest first ----
    let palette = [t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5];
    // one section per trip: the app's list lays out only the cards on screen
    let list = trips.iter().enumerate().rev().filter(|(i, _)| keep(*i)).map(|(i, tr)| {
        let expanded = open == Some(i);
        let total = tr.total_hkd().max(1.);
        // category proportions: a thin stacked bar, the top four named
        let bar = h_flex().h(px(6.)).w_full().rounded_full().overflow_hidden().bg(t.muted)
            .children(tr.by_category.iter().take(5).enumerate().map(|(k, (_, v))| {
                div().h_full().w(relative((v / total).clamp(0., 1.) as f32)).bg(palette[k % palette.len()])
            }));
        let legend = h_flex().gap_3().flex_wrap().children(tr.by_category.iter().take(4).enumerate().map(|(k, (c, v))| {
            crate::category::legend(palette[k % palette.len()], c, fmt::compact(*v), cx)
        }));
        let head = h_flex().gap_4().items_start()
            .child(v_flex().flex_1().min_w_0().gap(px(2.))
                .child(div().text_lg().font_semibold().overflow_hidden().text_ellipsis().whitespace_nowrap().child(tr.label()))
                .child(ui::caption(format!("{} · {} · {}", span(tr), plural(tr.days() as usize, "day"),
                    plural(tr.away_rows.len(), "purchase")), cx)))
            .child(v_flex().items_end().gap(px(2.)).flex_shrink_0()
                .child(div().text_lg().font_semibold().figures().text_color(fmt::money_out()).child(fmt::hkd(tr.total_hkd())))
                .child(ui::caption(format!("{}/day · booked {}", fmt::compact(tr.per_day_hkd()), fmt::compact(tr.booked_hkd)), cx)));
        let on_toggle = on_toggle.clone();
        let mut card = ui::card_interactive(("trip", i), cx)
            .on_click(move |_, window, cx| on_toggle(i, window, cx))
            .child(v_flex().gap_3().child(head).child(bar).child(legend));
        if expanded {
            if let Some(r) = atlas.routes.get(i) {
                card = card.child(div().mt_3().pt_3().border_t_1().border_color(ui::hairline(cx))
                    .child(transport(r, ledger, on_pick.clone(), cx)));
            }
            card = card.child(div().mt_3().pt_2().border_t_1().border_color(ui::hairline(cx))
                .child(rows(tr, ledger, on_pick.clone(), cx)));
            let (on_search, q) = (on_search.clone(), format!("trip:{}", tr.start));
            card = card.child(h_flex().mt_2().child(gpui_kit::component::button::Button::new(("trip-table", i)).xsmall().outline()
                .icon(gpui_kit::component::IconName::Search).label("Show these rows in Transactions")
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_search(q.clone(), window, cx)
                })));
        }
        card
    });

    ui::Sections::new()
        .gap(16.)
        .child(ui::with_info(ui::caption(format!("{} found since {}", plural(trips.len(), "trip"), day(&trips[0].start)), cx),
            ui::ts::CAPTION, "info-travel", EXPLAIN, cx))
        .child(year_bar(trips, year, on_year, cx))
        .child(map_card(trips, &atlas, open, &keep, &year.map(|y| format!("Where you went in {y}")).unwrap_or_else(|| "Where you went".into()), cx))
        .child(cards)
        .child(ui::chart_card("Cost per trip", &format!("on the ground + booked ahead · {}{}", year.map(|y| format!("{y} · ")).unwrap_or_default(),
            "click a trip below to highlight it"), 220., chart, cx))
        .child(ui::section("Trips", "newest first · click to see what was spent", cx))
        .children(list)
}

/// The trip's rows, oldest first; bookings tagged. A click opens the detail.
fn rows(tr: &Trip, ledger: &Ledger, on_pick: OnPick, cx: &App) -> Div {
    let t = cx.theme();
    let mut idx = tr.rows();
    idx.sort_by(|a, b| ledger.entries[*a].date.cmp(&ledger.entries[*b].date).then(b.cmp(a)));
    v_flex().children(idx.into_iter().filter_map(|i| {
        let e = ledger.entries.get(i)?;
        let booked = tr.booked_rows.contains(&i);
        let mine = drill::my_hkd(e).unwrap_or(e.amount);
        let on_pick = on_pick.clone();
        Some(
            h_flex()
                .id(("trip-row", i))
                .gap_3()
                .py_1()
                .px_1()
                .rounded(t.radius)
                .cursor_pointer()
                .hover(|s| s.bg(t.accent))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation(); // the card would collapse
                    on_pick(i, window, cx)
                })
                .child(div().w(px(84.)).text_xs().text_color(t.muted_foreground).child(e.date.clone()))
                .when_some(crate::logo::tile(crate::logo::for_entry(e), 22., cx), |d, l| d.child(l))
                .child(v_flex().flex_1().min_w_0()
                    .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(e.description.clone()))
                    .child(div().text_xs().text_color(t.muted_foreground).child(
                        format!("{} · {}{}", e.category, e.currency, if booked { " · booked ahead" } else { "" }))))
                .child(div().w(px(104.)).text_sm().text_right().whitespace_nowrap().figures().text_color(fmt::money_out()).child(fmt::hkd(-mine))),
        )
    }))
}

#[cfg(test)]
mod tests {
    // not super::*: that brings in gpui's own `test` attribute
    use super::{short_span, span, Trip};

    fn trip(start: &str, end: &str) -> Trip {
        Trip { start: start.into(), end: end.into(), places: vec![], home: "HK".into(), away_rows: vec![], booked_rows: vec![],
            away_hkd: 0., booked_hkd: 0., by_category: vec![] }
    }

    #[test]
    fn spans_read_naturally() {
        assert_eq!(span(&trip("2026-05-24", "2026-05-29")), "24 → 29 May 2026");
        assert_eq!(span(&trip("2026-06-12", "2026-07-22")), "12 Jun → 22 Jul 2026");
        assert_eq!(span(&trip("2025-12-23", "2026-01-04")), "23 Dec 2025 → 4 Jan 2026");
        assert_eq!(span(&trip("2025-06-29", "2025-06-29")), "29 Jun 2025");
        assert_eq!(short_span("2026-05-24", "2026-05-28"), "24 → 28 May");
        assert_eq!(short_span("2026-06-28", "2026-07-01"), "28 Jun → 1 Jul");
        assert_eq!(short_span("2026-06-28", "2026-06-28"), "28 Jun");
    }
}
