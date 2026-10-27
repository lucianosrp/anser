//! "Encrypt this computer's copy" (Settings, Sync) and the unlock screen a
//! passphrase-keyed replica asks for at start (finance_core::replica).
//!
//! * The switch: on seals finance.db, export.json and the personal config
//!   files ([`extra_files`]) with a key in the OS keyring; without a
//!   keyring it asks for a passphrase first. Off decrypts them back.
//! * Lost the key: the copy is a cache. "Start over" deletes it, and the
//!   next sync fetches everything again.

use std::path::PathBuf;

use finance_core::replica::{self, KeySource};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

/// The personal config files sealed along with the replica.
pub fn extra_files() -> Vec<PathBuf> {
    vec![crate::pages::investments::config_path(), crate::targets::path(), crate::downloads::path()]
}

pub enum VaultEvent {
    /// unlocked at start: load the replica
    Unlocked,
    /// the encrypted copy was dropped: sync again
    Resync,
}

pub struct Vault {
    status: replica::Status,
    keyring: Option<bool>,
    pass1: Entity<InputState>,
    pass2: Entity<InputState>,
    /// the passphrase form is open (turning on without a keyring)
    asking: bool,
    busy: bool,
    confirm_reset: bool,
    note: Option<(bool, String)>,
}

impl EventEmitter<VaultEvent> for Vault {}

impl Vault {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let masked = |ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph).masked(true))
        };
        // the keyring probe may wait on D-Bus: off the UI thread
        let probe = cx.background_spawn(async move { replica::keyring_available() });
        cx.spawn(async move |this, cx| {
            let ok = probe.await;
            let _ = this.update(cx, |this, cx| {
                this.keyring = Some(ok);
                cx.notify();
            });
        })
        .detach();
        Vault {
            status: replica::status(),
            keyring: None,
            pass1: masked("Passphrase", window, cx),
            pass2: masked("Once more", window, cx),
            asking: false,
            busy: false,
            confirm_reset: false,
            note: None,
        }
    }

    /// The wizard changed the copy (its Protect card): read the state again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.status = replica::status();
        cx.notify();
    }

    fn done(&mut self, r: Result<String, String>, cx: &mut Context<Self>) {
        self.busy = false;
        self.status = replica::status();
        self.note = Some(match r {
            Ok(s) => (true, s),
            Err(e) => (false, e),
        });
        cx.notify();
    }

    fn run(&mut self, cx: &mut Context<Self>, f: impl FnOnce() -> Result<String, String> + Send + 'static, after: Option<VaultEvent>) {
        self.busy = true;
        self.note = None;
        cx.notify();
        let task = cx.background_spawn(async move { f() });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                let ok = r.is_ok();
                this.done(r, cx);
                if let (true, Some(e)) = (ok, after) {
                    cx.emit(e);
                }
            });
        })
        .detach();
    }

    fn toggle(&mut self, on: bool, cx: &mut Context<Self>) {
        if !on {
            self.run(cx, || replica::disable(&extra_files()).map(|_| "Decrypted: the local copy is plain files again.".into()).map_err(|e| format!("{e:#}")), None);
            return;
        }
        if self.keyring == Some(true) {
            self.run(cx, || {
                replica::enable(None, &extra_files()).map(|_| "Encrypted. The key is in the OS keyring; nothing to type at start.".into()).map_err(|e| format!("{e:#}"))
            }, None);
        } else {
            self.asking = true;
            cx.notify();
        }
    }

    fn enable_with_passphrase(&mut self, cx: &mut Context<Self>) {
        let (a, b) = (self.pass1.read(cx).value().to_string(), self.pass2.read(cx).value().to_string());
        if a != b {
            self.note = Some((false, "The two passphrases differ.".into()));
            cx.notify();
            return;
        }
        if a.chars().count() < 8 {
            self.note = Some((false, "8 characters at least (four random words is better).".into()));
            cx.notify();
            return;
        }
        self.asking = false;
        self.run(cx, move || {
            replica::enable(Some(&a), &extra_files())
                .map(|_| "Encrypted. The app asks for this passphrase at every start.".into())
                .map_err(|e| format!("{e:#}"))
        }, None);
    }

    fn unlock(&mut self, cx: &mut Context<Self>) {
        let p = self.pass1.read(cx).value().to_string();
        if p.is_empty() {
            return;
        }
        self.run(cx, move || replica::unlock(&p).map(|_| "Unlocked.".into()).map_err(|e| format!("{e:#}")), Some(VaultEvent::Unlocked));
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.confirm_reset = false;
        self.run(cx, || {
            replica::forget(&extra_files()).map(|_| "The encrypted copy is gone; syncing again from the server.".into()).map_err(|e| format!("{e:#}"))
        }, Some(VaultEvent::Resync));
    }

    fn note_el(&self, cx: &App) -> Option<Div> {
        self.note.as_ref().map(|(ok, s)| {
            div().text_size(px(ts::CAPTION)).text_color(if *ok { cx.theme().muted_foreground } else { fmt::money_out() }).child(s.clone())
        })
    }

    fn reset_row(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        v_flex().gap_1()
            .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(
                "Lost the passphrase or the key? This copy is only a cache of the server: start over and the next sync fetches everything again."))
            .child(h_flex().gap_2().map(|d| if self.confirm_reset {
                d.child(Button::new("vault-reset-yes").small().danger().label("Delete the encrypted copy").loading(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.reset(cx))))
                    .child(Button::new("vault-reset-no").small().ghost().label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| { this.confirm_reset = false; cx.notify(); })))
            } else {
                d.child(Button::new("vault-reset").small().ghost().label("Start over…")
                    .on_click(cx.listener(|this, _, _, cx| { this.confirm_reset = true; cx.notify(); })))
            }))
    }

    /// The unlock screen (in place of every page while locked).
    pub fn unlock_screen(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let keyring_lost = self.status.via == Some(KeySource::Keyring);
        let card = ui::card(cx).w(px(420.)).gap_3()
            .child(div().text_lg().child("This computer's copy is encrypted"))
            .child(div().text_sm().text_color(t.muted_foreground).child(if keyring_lost {
                "Its key was kept in the OS keyring, and the keyring doesn't have it now (another login, a reset keyring)."
            } else {
                "Enter the passphrase you chose in Settings to open it."
            }))
            .when(!keyring_lost, |d| d
                .child(Input::new(&self.pass1).mask_toggle())
                .child(h_flex().child(Button::new("vault-unlock").small().primary().label("Unlock").loading(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.unlock(cx))))))
            .children(self.note_el(cx))
            .child(self.reset_row(cx));
        v_flex().size_full().items_center().justify_center().p_4().child(card).into_any_element()
    }

    /// The Settings card: the switch, the passphrase form, the state.
    pub fn settings_card(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let on = self.status.encrypted;
        let switch = Switch::new("vault-switch").with_size(ui::switch_size())
            .checked(on || self.asking)
            .label("Encrypt this computer's copy")
            .disabled(self.busy || (!on && self.keyring.is_none()))
            .on_click(cx.listener(|this, v: &bool, _, cx| {
                if this.asking && !*v {
                    this.asking = false;
                    cx.notify();
                } else {
                    this.toggle(*v, cx)
                }
            }));
        let state = if on { self.status.label() } else { "off: the replica, the investment plan and the targets are plain files".into() };
        let mut card = ui::card(cx).gap_2()
            .child(h_flex().gap_3().items_center().child(switch).when(self.busy, |d| d.child(ui::caption("working…", cx))))
            .child(div().text_size(px(ts::CAPTION)).text_color(if on { fmt::money_in() } else { t.muted_foreground }).child(state));
        if self.asking {
            let (a, b) = (self.pass1.read(cx).value().to_string(), self.pass2.read(cx).value().to_string());
            let (_, hint) = finance_core::backups::strength(&a);
            card = card
                .child(div().text_size(px(ts::CAPTION)).text_color(t.warning).child(
                    "There's no OS keyring here, so the key comes from a passphrase you type at every start. Forget it and the copy is unreadable (it can always be synced again)."))
                .child(h_flex().gap_2().flex_wrap()
                    .child(div().w(px(200.)).child(Input::new(&self.pass1).small().mask_toggle()))
                    .child(div().w(px(200.)).child(Input::new(&self.pass2).small().mask_toggle())))
                .child(ui::caption(if b.is_empty() || a == b { hint.to_string() } else { "the two differ".into() }, cx))
                .child(h_flex().gap_2()
                    .child(Button::new("vault-enable").small().primary().label("Encrypt").on_click(cx.listener(|this, _, _, cx| this.enable_with_passphrase(cx))))
                    .child(Button::new("vault-cancel").small().ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| { this.asking = false; cx.notify(); }))));
        }
        card = card.children(self.note_el(cx))
            .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(
                "This computer's disk isn't encrypted, so this seals the local copy: finance.db, export.json, invest.toml, targets.toml and downloads.toml. \
                 Not covered: theme.toml, the formats folder, and the files you upload from your own folders. The key stays in the OS keyring (or comes from your passphrase), never next to the data."));
        if on {
            card = card.child(self.reset_row(cx));
        }
        card.into_any_element()
    }
}

/// Locked: the unlock screen (the app shows it in place of the pages);
/// otherwise the Settings card.
impl Render for Vault {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if replica::locked() {
            return self.unlock_screen(cx);
        }
        // made before the app unlocked the copy (or another profile's): read again
        if self.status.encrypted && !self.status.unlocked {
            self.status = replica::status();
        }
        self.settings_card(cx)
    }
}
