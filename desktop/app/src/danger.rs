//! The danger zone's confirmation popup: what will happen, in plain words,
//! then Cancel and a red button. The worst ones also ask for a phrase to be
//! typed, so a stray click or Enter can't run them.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::zoom::px;

pub type OnConfirm = Rc<dyn Fn(&mut Window, &mut App)>;

/// Open the popup. `typed`: a phrase the red button waits for.
pub fn confirm(title: &str, body: String, action: &str, typed: Option<&'static str>, on_ok: OnConfirm, window: &mut Window, cx: &mut App) {
    let input = typed.map(|t| cx.new(|cx| InputState::new(window, cx).placeholder(t)));
    let (title, action) = (SharedString::from(title.to_string()), SharedString::from(action.to_string()));
    let focus = input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = cx.theme();
        let ready = match (&input, typed) {
            (Some(i), Some(p)) => i.read(cx).value().trim() == p,
            _ => true,
        };
        let (go, inp) = (on_ok.clone(), input.clone());
        dialog.w(px(460.)).title(title.clone()).child(
            v_flex().gap_3()
                .child(div().text_sm().text_color(t.muted_foreground).whitespace_normal().child(body.clone()))
                .when_some(input.clone().zip(typed), |d, (i, p)| d
                    .child(div().text_sm().child(format!("Type {p} to confirm.")))
                    .child(Input::new(&i).small()))
                .child(h_flex().gap_2().justify_end().pt_2()
                    .child(Button::new("dz-cancel").small().ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("dz-go").small().danger().label(action.clone()).disabled(!ready)
                        .on_click(move |_, window, cx| {
                            let ok = match (&inp, typed) {
                                (Some(i), Some(p)) => i.read(cx).value().trim() == p,
                                _ => true,
                            };
                            if ok {
                                window.close_dialog(cx);
                                go(window, cx);
                            }
                        }))),
        )
    });
    if let Some(i) = focus {
        i.update(cx, |s, cx| s.focus(window, cx));
    }
}
