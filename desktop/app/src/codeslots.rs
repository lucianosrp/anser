//! A pairing code typed like a PIN: eight slots, the dash already there.
//!
//! The slots are drawn over an ordinary text input (invisible, on top), so
//! typing, Ctrl+V and the phone keyboard all work as in any input. Next to
//! them a Paste button reads the clipboard itself: a phone has no shortcut
//! and no visible field to long-press for the system's Paste. [`clean`]
//! keeps only the code's own characters; the caller signs in once
//! [`complete`] says all eight are there, however they got in.

use finance_core::signin;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, ActiveTheme as _, Sizable as _};
use gpui_kit::*;

use crate::zoom::px;

gpui_kit::assets::icon_assets!(pub CodeIcons, [ClipboardPaste]);

/// After an input changed: tidy it to the code's characters (upper case,
/// at most eight). `prev` is the whole code the slots held before (the web's
/// browser paste inserts at the caret): a whole code pasted over it replaces
/// it. Returns the cleaned code.
pub fn clean(state: &Entity<InputState>, prev: Option<&str>, window: &mut Window, cx: &mut App) -> String {
    let typed = state.read(cx).value().to_string();
    let code = prev.and_then(|p| pasted_over(p, &typed)).unwrap_or_else(|| signin::clean_code(&typed));
    if code != typed {
        let c = code.clone();
        state.update(cx, |s, cx| s.set_value(c, window, cx));
    }
    code
}

/// `typed` is a whole code `prev` was edited into by inserting text (a paste
/// at the caret): that text, when it is a whole code by itself.
fn pasted_over(prev: &str, typed: &str) -> Option<String> {
    let (p, t): (Vec<char>, Vec<char>) = (prev.chars().collect(), typed.chars().collect());
    if t.len() <= p.len() {
        return None;
    }
    let pre = p.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let suf = p.iter().rev().zip(t.iter().rev()).take_while(|(a, b)| a == b).count().min(p.len() - pre);
    let inserted: String = t[pre..t.len() - suf].iter().collect();
    signin::pasted_code(&inserted).ok().filter(|c| complete(c))
}

/// All eight characters are in.
pub fn complete(code: &str) -> bool {
    code.chars().count() == signin::CODE_LEN
}

/// Ctrl+V in the slots, on the web, is the browser's own paste: its paste
/// event hands the text over without asking. The input's Paste action would
/// go through the Clipboard API instead, which needs a permission the
/// browser may refuse, and then nothing at all is pasted. (Cmd+V in a Mac
/// browser already goes that way; the desktop keeps the action.)
pub fn bind_keys(_cx: &mut App) {
    #[cfg(target_family = "wasm")]
    _cx.bind_keys([KeyBinding::new("ctrl-v", NoAction, Some("CodeSlots > Input"))]);
}

/// A pasted code replaces what the slots held (inserted at the caret, a
/// paste after a typo would make another wrong code). Through `replace_all`,
/// so the input says Change and the caller's subscription signs in exactly
/// as for typing.
fn put(state: &Entity<InputState>, code: String, window: &mut Window, cx: &mut App) {
    let done = complete(&code);
    state.update(cx, |s, cx| {
        s.replace_all(code, window, cx);
        // the rest is still to type; a whole code signs in (no phone keyboard popping up)
        if !done {
            s.focus(window, cx);
        }
    });
}

/// The Paste button: read the clipboard and put the code in the slots. The
/// read starts here, inside the click, as the browser wants (on the web it
/// is the asynchronous Clipboard API, which may ask for permission); the
/// task ends with the note to show when nothing went in.
pub fn paste(state: &Entity<InputState>, window: &mut Window, cx: &mut App) -> Task<Result<(), &'static str>> {
    let read = cx.read_from_clipboard_async();
    let state = state.clone();
    window.spawn(cx, async move |cx| {
        let read = read.await;
        // FINANCE_DESKTOP_DEBUG_CLIPBOARD=1: what the read returned (its shape,
        // never the code itself), for "the clipboard holds no code" reports
        if std::env::var_os("FINANCE_DESKTOP_DEBUG_CLIPBOARD").is_some() {
            let shape = |t: &str| t.chars().map(|c| if c.is_ascii_alphanumeric() { 'x' } else { c }).collect::<String>();
            match &read {
                Ok(Some(i)) => eprintln!("[clipboard] item: {} entries, text: {:?}", i.entries().len(), i.text().map(|t| shape(&t))),
                Ok(None) => eprintln!("[clipboard] read returned None (no offer, or nothing readable)"),
                Err(e) => eprintln!("[clipboard] read error: {e:?}"),
            }
        }
        let text = match read {
            Ok(item) => item.and_then(|i| i.text()).unwrap_or_default(),
            Err(_) => return Err(signin::CLIPBOARD_UNREADABLE),
        };
        let code = signin::pasted_code(&text)?;
        cx.update(|window, cx| put(&state, code, window, cx)).map_err(|_| signin::CLIPBOARD_UNREADABLE)
    })
}

/// The slots and their Paste button; `on_paste` is the page's, so it can
/// show [`paste`]'s note.
pub fn render(
    id: &'static str,
    state: &Entity<InputState>,
    on_paste: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let t = cx.theme();
    let code: Vec<char> = state.read(cx).value().chars().collect();
    let next = code.len();
    let slot = |i: usize| {
        let c = code.get(i).copied();
        div().w(px(34.)).h(px(44.)).flex().items_center().justify_center().rounded(px(8.))
            .border_1()
            .border_color(if i == next { t.primary } else { t.border })
            .bg(t.secondary)
            .text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).font_family(t.mono_font_family.clone())
            .text_color(t.foreground)
            .children(c.map(|c| c.to_string()))
    };
    let half = signin::CODE_LEN / 2;
    let focus = state.clone();
    let keys = state.clone();
    let slots = div().id(id).key_context("CodeSlots").relative().w(px(34. * 8. + 6. * 6. + 28.))
        // a press anywhere on the slots puts the caret in the input
        .on_mouse_down(MouseButton::Left, move |_, window, cx| focus.update(cx, |s, cx| s.focus(window, cx)))
        .child(h_flex().gap(px(6.)).items_center()
            .children((0..half).map(slot))
            .child(div().w(px(16.)).text_center().text_color(t.muted_foreground).child("-"))
            .children((half..signin::CODE_LEN).map(slot)))
        // the real input, on top and invisible. On the desktop Ctrl+V of a
        // code replaces the slots; on the web the browser's paste inserts it
        // at the caret (bind_keys) and `clean` tidies it
        .child(div().absolute().top_0().left_0().size_full().opacity(0.)
            .child(Input::new(state).appearance(false).h_full().on_paste(move |item, window, cx| {
                match item.text().map(|t| signin::pasted_code(&t)) {
                    Some(Ok(code)) => {
                        put(&keys, code, window, cx);
                        true
                    }
                    _ => false,
                }
            })));
    h_flex().gap(px(10.)).items_center().justify_center().flex_wrap()
        .child(slots)
        .child(Button::new(SharedString::from(format!("{id}-paste"))).small().outline()
            .icon(Lucide::ClipboardPaste).label("Paste")
            .tooltip("Paste the code from the clipboard")
            .on_click(on_paste))
}

#[cfg(test)]
mod tests {
    use super::pasted_over;

    #[test]
    fn a_whole_code_pasted_over_the_old_one_replaces_it() {
        assert_eq!(pasted_over("ABCDEFGH", "ABCDEFGHJKMNPQRS").as_deref(), Some("JKMNPQRS"));
        assert_eq!(pasted_over("ABCDEFGH", "JKMNPQRSABCDEFGH").as_deref(), Some("JKMNPQRS"));
        assert_eq!(pasted_over("ABCDEFGH", "ABCDJKMN-PQRSEFGH").as_deref(), Some("JKMNPQRS"));
        assert_eq!(pasted_over("ABCD", "ABCDJKMNPQRS").as_deref(), Some("JKMNPQRS"));
    }

    #[test]
    fn typing_is_not_a_paste() {
        assert_eq!(pasted_over("ABCDEFGH", "ABCDEFGHJ"), None);
        assert_eq!(pasted_over("ABCD", "ABCDE"), None);
        assert_eq!(pasted_over("ABCDEFGH", "ABCDEFG"), None);
    }
}
