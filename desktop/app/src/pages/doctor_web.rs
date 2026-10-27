//! Doctor, web build: the checks probe DNS, the GPU, the replica and local
//! files, none of which a browser tab can reach, so the page points at the
//! desktop app for those. It does show the Sources check: what the server
//! has, from the capabilities in the bundle the page loaded. Same public
//! surface as `doctor.rs` (the app wires both identically).

use finance_core::capabilities::Capabilities;
use gpui_kit::component::{ActiveTheme as _, IconName, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;

use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

/// Asks the app to sync, or to open Upload (never, here).
pub enum DoctorEvent {
    SyncNow,
    OpenUpload,
}

pub struct Doctor;

impl EventEmitter<DoctorEvent> for Doctor {}

impl Doctor {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Doctor
    }

    pub fn run(&mut self, _cx: &mut Context<Self>) {}

    pub fn synced(&mut self, _cx: &mut Context<Self>) {}
}

impl Render for Doctor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let caps = finance_core::Bundle::load(std::path::Path::new("")).map(|b| Capabilities::of(&b)).unwrap_or_default();
        let r = caps.report(chrono::Utc::now().timestamp_millis());
        let t = cx.theme().clone();
        let color = match r.level {
            0 => fmt::money_in(),
            2 => t.primary,
            _ => t.warning,
        };
        let label = match r.level {
            0 => "ok",
            2 => "not set up",
            _ => "warn",
        };
        let mut rows = r.rows;
        rows.push(("App".into(), finance_core::VERSION.into()));
        let facts = v_flex().gap(px(2.)).children(rows.into_iter().map(|(k, v)| {
            h_flex().gap_3().items_start().text_size(px(ts::CAPTION))
                .child(div().w(px(110.)).flex_shrink_0().text_color(t.muted_foreground).child(k))
                .child(div().flex_1().min_w_0().child(v))
        }));
        v_flex()
            .w_full()
            .gap(px(ui::sp::GRID))
            .child(ui::card(cx).gap_2()
                .child(h_flex().gap_2().items_center()
                    .child(ui::dot(color, 9.))
                    .child(div().text_size(px(ts::TITLE)).font_semibold().child("Sources"))
                    .child(ui::chip(label, color, cx)))
                .child(div().text_size(px(ts::BODY)).child(r.summary))
                .child(facts))
            .child(ui::empty_with(
                IconName::Settings,
                "The other checks run in the desktop app",
                "Tailnet, replica, GPU and installs need the machine itself; a browser tab can't see them.",
                cx,
            ))
    }
}
