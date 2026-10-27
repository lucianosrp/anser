//! The avatar menu in the sidebar header: whose data this is, and the way
//! to another account.
//!
//! * Desktop: every account on this machine (finance_core::profiles: one
//!   per server and user, each with its own local copy and token), a tick
//!   on the one in use; picking another switches to it. *Add an account…*
//!   asks for the server and signs in there (a pairing code, or an invite
//!   on the sign-in screen; closing the wizard before signing in, or the
//!   next start, drops it: profiles::prune_unfinished). Unfinished
//!   accounts are never listed. *Sign out…* forgets this account's token and
//!   its local copy (the server keeps the data). Switching restarts the app
//!   on the other account: every page, cache and key starts clean, so
//!   nothing of one account's data is left on screen for the next.
//! * Browser: who is signed in, and *Sign out…* (the session cookie ends;
//!   the sign-in screen comes back).
//!
//! Signing out (both, and Settings, Account, Device): a confirmation, then the
//! server signs this device out and hands back a code to jump back in,
//! shown with Copy; Done leaves. The code works only on this device (with
//! its resume key, finance_core::signin), once, with no time limit. *Sign
//! out for good…* (a shared or lost device) leaves no code.
//!
//! With one account and one user the header reads as it always did.

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    popover::Popover,
    h_flex, v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

use super::FinanceApp;
use crate::ui;
use crate::zoom::px;

/// Start this program again (same arguments): the active account changed.
#[cfg(not(target_family = "wasm"))]
pub fn relaunch() -> ! {
    relaunch_with(false)
}

/// Set by [`relaunch_with`] for the next start only: open the wizard.
pub const SETUP_ENV: &str = "FINANCE_DESKTOP_SETUP";

/// Set by *Add an account…* for the next start only: the profile it made,
/// which the start's pruning of unfinished accounts keeps (main.rs). Any
/// later start without it signed in drops it.
pub const ADDING_ENV: &str = "FINANCE_DESKTOP_ADDING";

/// [`relaunch`], and with `setup` the new start opens the wizard (leaving
/// the demo for real). The flag never outlives that one start.
#[cfg(not(target_family = "wasm"))]
pub fn relaunch_with(setup: bool) -> ! {
    relaunch_env(setup, None)
}

#[cfg(not(target_family = "wasm"))]
fn relaunch_env(setup: bool, adding: Option<&str>) -> ! {
    // exec doesn't run Drop: stop the local anser-server this app owns first
    finance_core::local_server::stop_owned();
    // exe_path: after an update replaced the file, current_exe ends in " (deleted)"
    if let Ok(exe) = finance_core::selfupdate::exe_path() {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(&args);
        if setup {
            cmd.env(SETUP_ENV, "1");
        } else {
            cmd.env_remove(SETUP_ENV);
        }
        match adding {
            Some(id) => cmd.env(ADDING_ENV, id),
            None => cmd.env_remove(ADDING_ENV),
        };
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let e = cmd.exec();
            eprintln!("restarting on the other account failed: {e}");
        }
        #[cfg(not(unix))]
        {
            let _ = cmd.spawn();
        }
    }
    std::process::exit(0)
}

impl FinanceApp {
    /// The header's title and the line under it: the signed-in user's name
    /// (whoami, else this machine's profile label) over the server's short
    /// host; "Anser" over "local-first" while no name is known.
    pub(super) fn brand_lines(&self, cx: &App) -> (String, String) {
        let name = self.acct.account.read(cx).who().and_then(|w| w.user).map(|u| u.name).filter(|n| !n.is_empty());
        #[cfg(not(target_family = "wasm"))]
        {
            use finance_core::profiles;
            let p = profiles::active();
            if profiles::is_demo(&p) {
                return (name.unwrap_or_else(|| "Anser".into()), finance_core::demo::LABEL.into());
            }
            // "myserver.tail1234.ts.net" -> "myserver" (an IP stays whole)
            let host = profiles::short_host(&p.url);
            if let Some(n) = name.clone().or_else(|| Some(p.label.clone()).filter(|l| !l.is_empty())) {
                return (n, if host.is_empty() { "local-first".into() } else { host.into() });
            }
        }
        // the browser: the demo's label, else the server it was loaded from
        #[cfg(target_family = "wasm")]
        if let Some(n) = name.clone() {
            if finance_core::demo::active() {
                return (n, finance_core::demo::LABEL.into());
            }
            let url = finance_core::cache::server_url();
            let host = finance_core::client::short_host(&url);
            if !host.is_empty() {
                return (n, host.into());
            }
        }
        match name {
            Some(n) => (n, "local-first".into()),
            None => ("Anser".into(), "local-first".into()),
        }
    }

    /// The header's brand (avatar, name) as the menu's trigger.
    pub(super) fn account_menu(&self, brand: AnyElement, rail: bool, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        let who = self.acct.account.read(cx).who();
        #[cfg(not(target_family = "wasm"))]
        let accounts: Vec<(String, String, bool)> = {
            use finance_core::profiles;
            let active = profiles::active().id;
            // never an unfinished account (pruned at start; menu_list hides one that slips through)
            profiles::menu_list().into_iter().map(|p| (p.id.clone(), profiles::display(&p), p.id == active)).collect()
        };
        let trigger = Button::new("account-menu")
            .ghost()
            .p_0()
            .h(px(38.))
            .when(!rail, |b| b.w_full().justify_start())
            .child(brand);
        // dev/screenshot aid: FINANCE_DESKTOP_ACCOUNT_MENU=1 opens it
        let open = super::dev_aid("FINANCE_DESKTOP_ACCOUNT_MENU").is_some_and(|v| v == "1");
        Popover::new("account-menu-pop")
            .anchor(Anchor::TopLeft)
            .default_open(open)
            .trigger(trigger)
            .when(!rail, |p| p.trigger_style(StyleRefinement::default().w_full()))
            .content(move |_, _, cx| {
                let t = cx.theme().clone();
                let pop = cx.entity().downgrade();
                // a row that closes the menu, then acts
                let row = |id: &'static str, ix: usize, label: String, icon: Option<IconName>, on: bool, act: Rc<dyn Fn(&mut Window, &mut App)>| {
                    let pop = pop.clone();
                    h_flex().id((id, ix)).gap_2().items_center().h(px(30.)).px_2().rounded(t.radius).cursor_pointer()
                        .text_sm()
                        .hover(|s| s.bg(t.accent))
                        .child(div().w(px(16.)).flex_shrink_0().when_some(icon.or(on.then_some(IconName::Check)), |d, i| d.child(Icon::new(i).small())))
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .on_click(move |_, window, cx| {
                            let _ = pop.update(cx, |s, cx| s.dismiss(window, cx));
                            act(window, cx);
                        })
                };
                let caption = |s: String| div().px_2().pt_1().text_xs().text_color(t.muted_foreground).child(s);
                let sep = || div().my_1().h(px(1.)).bg(t.border);
                let mut col = v_flex().w(px(280.)).p_1().gap(px(1.));
                if let Some(u) = who.as_ref().and_then(|w| w.user.clone()) {
                    col = col.child(div().px_2().pt_1().text_sm().font_semibold().child(u.name.clone()))
                        .child(caption(format!("signed in{}", if u.role == "admin" { ", admin of this server" } else { "" })));
                }
                // the demo: what it is, and the way out (no sign-out: nobody signed in)
                if super::demo::active() {
                    return col.child(sep()).child(caption(finance_core::demo::NOTE.into()))
                        .child(row("demo-real", 0, "Set up for real…".into(), Some(IconName::Plus), false, Rc::new(|_, _| super::demo::leave(true))))
                        .child(row("demo-leave", 0, "Leave the demo".into(), Some(IconName::Close), false, Rc::new(|_, _| super::demo::leave(false))));
                }
                #[cfg(not(target_family = "wasm"))]
                {
                    if accounts.len() > 1 {
                        col = col.child(sep()).child(caption("Accounts on this computer".into()));
                        for (i, (id, label, on)) in accounts.iter().cloned().enumerate() {
                            col = col.child(row("acct-switch", i, label, None, on, Rc::new(move |_, _| {
                                if !on && finance_core::profiles::set_active(&id).is_ok() {
                                    relaunch();
                                }
                            })));
                        }
                    }
                    let v = view.clone();
                    col = col.child(sep()).child(row("acct-add", 0, "Add an account…".into(), Some(IconName::Plus), false, Rc::new(move |window, cx| {
                        let _ = v.update(cx, |this, cx| this.add_account(window, cx));
                    })));
                    // look around with made-up data (its own account, deleted on leaving)
                    col = col.child(row("acct-demo", 0, "Just try it (made-up data)".into(), Some(IconName::Eye), false, Rc::new(|_, cx| {
                        super::demo::start(cx, |e, _| eprintln!("anser: {e}"));
                    })));
                }
                #[cfg(target_family = "wasm")]
                {
                    col = col.child(sep());
                }
                let (v, v2) = (view.clone(), view.clone());
                col.child(row("acct-out", 0, "Sign out…".into(), None, false, Rc::new(move |window, cx| {
                    let _ = v.update(cx, |this, cx| this.sign_out(window, cx));
                })))
                .child(row("acct-out-good", 0, "Sign out for good…".into(), None, false, Rc::new(move |window, cx| {
                    let _ = v2.update(cx, |this, cx| this.sign_out_as(true, window, cx));
                })))
            })
            .into_any_element()
    }

    /// Another account: a server (this one by default), then the sign-in
    /// screen on it after the restart.
    pub(super) fn add_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_family = "wasm")]
        {
            let _ = (window, cx);
        }
        #[cfg(not(target_family = "wasm"))]
        {
            let url = finance_core::cache::server_url();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("https://your-server.tail1234.ts.net").default_value(url));
            let note: Entity<Option<String>> = cx.new(|_| None);
            window.open_dialog(cx, move |dialog, _, cx| {
                let (input, note) = (input.clone(), note.clone());
                let err = note.read(cx).clone();
                dialog.w(px(520.)).title("Add an account").child(
                    v_flex().gap_3()
                        .child(ui::caption("Another person's account on this server, or an account on another server. \
                            It gets its own local copy and its own sign-in; the app restarts on it, on the sign-in screen \
                            (a pairing code, or an invite to make the account).", cx))
                        .child(Input::new(&input).small())
                        .children(err.map(|e| div().text_size(px(12.)).text_color(crate::fmt::money_out()).child(e)))
                        .child(Button::new("acct-add-go").small().primary().label("Add and sign in").on_click(move |_, _, cx| {
                            let url = input.read(cx).value().trim().to_string();
                            match finance_core::profiles::add(&url, "").and_then(|p| finance_core::profiles::set_active(&p.id).map(|()| p)) {
                                Ok(p) => relaunch_env(false, Some(&p.id)),
                                Err(e) => note.update(cx, |n, cx| {
                                    *n = Some(format!("{e:#}"));
                                    cx.notify();
                                }),
                            }
                        })),
                )
            });
        }
    }

    /// Sign out of the account in use, with a code to jump back in.
    pub(super) fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sign_out_as(false, window, cx);
    }

    /// Sign out of the account in use, after a confirmation: the server
    /// signs this device out (with a code to jump back in unless `for_good`).
    /// Then the browser's session is over; the desktop forgets the token and
    /// the local copy (another account becomes the one in use, if there is
    /// one).
    pub(super) fn sign_out_as(&mut self, for_good: bool, window: &mut Window, cx: &mut Context<Self>) {
        let here = if cfg!(target_family = "wasm") { "this browser" } else { "this computer" };
        let gone = if cfg!(target_family = "wasm") {
            "The session in this browser ends. The server keeps everything."
        } else {
            "This computer forgets its device token for the account and its local copy of the data. The server keeps everything."
        };
        #[cfg(not(target_family = "wasm"))]
        let title = format!("Sign out of {}?", finance_core::profiles::display(&finance_core::profiles::active()));
        #[cfg(target_family = "wasm")]
        let title = "Sign out?".to_string();
        let (title, body, action) = if for_good {
            (title.replace("Sign out", "Sign out for good"),
                format!("{gone} No code is given: signing in again on {here} takes a new pairing code. For a shared or lost device."),
                "Sign out for good")
        } else {
            (title, format!("{gone} You get a code to jump back in on {here}: it works only here, once, and doesn't expire."), "Sign out")
        };
        let view = cx.entity().downgrade();
        crate::danger::confirm(&title, body, action, None, Rc::new(move |window, cx| {
            let _ = view.update(cx, |this, cx| this.sign_out_now(for_good, window, cx));
        }), window, cx);
    }

    /// The confirmed sign-out: the server first (the token is still good),
    /// then the code, then leaving.
    fn sign_out_now(&mut self, for_good: bool, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(not(target_family = "wasm"))]
        {
            let p = finance_core::profiles::active();
            let server = finance_core::cache::server_url();
            let task = cx.background_spawn(async move { finance_core::auth::logout(&server, !for_good) });
            cx.spawn_in(window, async move |this, cx| {
                let r = task.await;
                let _ = this.update_in(cx, |_, window, cx| {
                    // this computer lets go of the account
                    let leave: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, _| {
                        let _ = finance_core::auth::forget_key(&p.token_key);
                        if finance_core::profiles::list().len() > 1 {
                            let _ = finance_core::profiles::remove(&p.id);
                        } else {
                            let _ = std::fs::remove_dir_all(finance_core::profiles::dir_of(&p.id));
                        }
                        relaunch();
                    });
                    match r {
                        Ok(Some(code)) => show_code(code, leave, window, cx),
                        // signed out, no code (for good, or a server older than codes)
                        Ok(None) => leave(window, cx),
                        Err(e) => crate::danger::confirm("The server didn't answer", format!("{e:#}. Sign out on this computer only? \
                            The server keeps this device's token valid until an admin signs it out (Settings, Admin, Users)."),
                            "Sign out here", None, leave, window, cx),
                    }
                });
            })
            .detach();
        }
        #[cfg(target_family = "wasm")]
        {
            let mut body = serde_json::json!({});
            if !for_good {
                if let Some(k) = finance_core::signin::ensure_resume_key() {
                    body["resume_key"] = k.into();
                }
            }
            let fut = finance_core::client::send(finance_core::client::Request::new("POST", "/api/logout").json(&body));
            cx.spawn_in(window, async move |this, cx| {
                let r = fut.await;
                let _ = this.update_in(cx, |_, window, cx| {
                    // the session is over: the page again, on the sign-in screen
                    let leave: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(|_, _| {
                        finance_core::signin::user_action();
                        let _ = finance_core::sync::pull();
                    });
                    match crate::pages::sources::json_of(r) {
                        Ok(v) => match v["reconnect_code"].as_str().filter(|c| !c.is_empty()) {
                            Some(code) => show_code(code.to_string(), leave, window, cx),
                            None => leave(window, cx),
                        },
                        Err(e) => {
                            let msg = format!("{e}. You are still signed in: try again in a moment.");
                            window.open_dialog(cx, move |d, _, cx| d.w(px(420.)).title("The server didn't answer").child(ui::caption_wrap(msg.clone(), cx)));
                        }
                    }
                });
            })
            .detach();
        }
    }
}

/// The code to jump back in, with Copy. Only Done closes it, then `done`
/// (the sign-out finishes): the code is never gone before it was seen.
fn show_code(code: String, done: Rc<dyn Fn(&mut Window, &mut App)>, window: &mut Window, cx: &mut App) {
    let shown = finance_core::signin::format_code(&code);
    let here = if cfg!(target_family = "wasm") { "this browser" } else { "this computer" };
    let body = format!("To sign in again on {here}, enter it where a pairing code goes. It works only here, once, and doesn't expire. \
        Keep it somewhere safe (a password manager); anywhere else it is useless.");
    // Copy says "Copied" for a moment; the dialog's builder runs again on refresh
    let flash = Rc::new(std::cell::Cell::new(false));
    window.open_dialog(cx, move |dialog, _, cx| {
        let (shown, copied, done, flash) = (shown.clone(), shown.clone(), done.clone(), flash.clone());
        let was_copied = flash.get();
        dialog.w(px(480.)).title("Here's the code to jump back in").close_button(false).overlay_closable(false).keyboard(false).child(
            v_flex().gap_3()
                .child(ui::caption_wrap(body.clone(), cx))
                .child(h_flex().gap_3().items_center()
                    .child(div().text_xl().font_semibold().font_family(cx.theme().mono_font_family.clone()).child(shown))
                    .child(Button::new("acct-signout-copy").xsmall().outline()
                        .icon(if was_copied { IconName::Check } else { IconName::Copy })
                        .label(if was_copied { "Copied" } else { "Copy" })
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                            flash.set(true);
                            window.refresh();
                            let flash = flash.clone();
                            window.spawn(cx, async move |cx| {
                                cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
                                flash.set(false);
                                let _ = cx.update(|window, _| window.refresh());
                            })
                            .detach();
                        })))
                .child(h_flex().justify_end().pt_2()
                    .child(Button::new("acct-signout-done").small().primary().label("Done").on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        done(window, cx);
                    }))),
        )
    });
}
