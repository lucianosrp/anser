//! The window's side of the server account: the Settings account card
//! (pages::account), the onboarding wizard (pages::onboarding) and when it
//! opens by itself, signing in when the server turns this device away, and
//! the user settings and avatar taken from each bundle (settings_sync,
//! crate::avatar). Kept out of app.rs, which only calls in.
//!
//! Turned away (a 401 on the data, `finance_core::signin`): the wizard
//! opens on its sign-in step, in the browser always (it has no data
//! without the server), on the desktop when there's no local copy to show.
//! Nothing navigates or reloads; after signing in, the data is fetched
//! once (the browser in the page, the desktop by a sync).

use finance_core::capabilities::Capabilities;
use finance_core::{signin, Bundle};
use gpui_kit::*;

use super::{FinanceApp, SyncNow};
use crate::pages::{self, account::AccountEvent, backups::BackupsEvent, onboarding::OnboardingEvent, vault::VaultEvent, Page};

pub struct Glue {
    pub account: Entity<pages::account::Account>,
    pub onboarding: Option<Entity<pages::onboarding::Onboarding>>,
    /// the wizard covers the page
    pub open: bool,
    /// it opened by itself once this session (closing it sticks)
    auto_done: bool,
    /// the encrypted local copy: Settings' switch, and the unlock screen
    pub vault: Entity<pages::vault::Vault>,
    /// Settings, Backups
    pub backups: Entity<pages::backups::Backups>,
    /// Settings, AI assistants
    pub assistants: Entity<pages::assistants::Assistants>,
}

impl Glue {
    pub fn new(window: &mut Window, cx: &mut Context<FinanceApp>) -> (Glue, Vec<Subscription>) {
        let account = cx.new(|cx| pages::account::Account::new(window, cx));
        let sub = cx.subscribe_in(&account, window, |this, _, ev: &AccountEvent, window, cx| match ev {
            AccountEvent::Resync => this.sync_now(&SyncNow, cx),
            AccountEvent::RunSetup => this.open_onboarding(true, window, cx),
            AccountEvent::SignedIn => this.after_sign_in(cx),
            AccountEvent::SignOut(for_good) => this.sign_out_as(*for_good, window, cx),
            AccountEvent::SetUpSource(key) => {
                this.settings_tab = pages::settings::Tab::Data;
                this.acct.account.update(cx, |a, cx| a.open_source_form(key, window, cx));
                cx.notify();
            }
            AccountEvent::OpenUpload => this.go(Page::Upload, window, cx),
        });
        // dev/screenshot aid: FINANCE_DESKTOP_ONBOARDING=1 (the browser:
        // ?onboarding=1) opens the wizard
        // ... and so does leaving the demo for real (accounts_menu::relaunch_with)
        let setup = std::env::var_os(super::accounts_menu::SETUP_ENV).is_some() && !super::demo::active();
        if pages::onboarding::dev_aid().is_some() || setup {
            cx.defer_in(window, |this, window, cx| this.open_onboarding(true, window, cx));
        } else if signin::needed() {
            // the browser's boot fetch was turned away: sign in first
            cx.defer_in(window, |this, window, cx| this.open_sign_in(window, cx));
        }
        let vault = cx.new(|cx| pages::vault::Vault::new(window, cx));
        let vsub = cx.subscribe(&vault, |this, _, ev: &VaultEvent, cx| match ev {
            VaultEvent::Unlocked => this.load_cache(true, cx),
            VaultEvent::Resync => {
                this.store = None;
                this.sync_now(&SyncNow, cx)
            }
        });
        let backups = cx.new(|cx| pages::backups::Backups::new(window, cx));
        let bsub = cx.subscribe(&backups, |this, _, ev: &BackupsEvent, cx| match ev {
            BackupsEvent::Imported => this.sync_now(&SyncNow, cx),
        });
        let assistants = cx.new(|cx| pages::assistants::Assistants::new(window, cx));
        (Glue { account, onboarding: None, open: false, auto_done: false, vault, backups, assistants }, vec![sub, vsub, bsub])
    }
}

impl FinanceApp {
    /// The wizard, over the whole window. `closable`: opened from the app
    /// (Settings, the no-data page), so its X leaves; by itself it has none.
    pub(super) fn open_onboarding(&mut self, closable: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.acct.onboarding.is_none() {
            let o = cx.new(|cx| pages::onboarding::Onboarding::new(closable, window, cx));
            self.watch_onboarding(&o, window, cx);
            self.acct.onboarding = Some(o);
        }
        self.show_onboarding(window, cx);
    }

    /// Nothing else on screen: no sheet or dialog left over it, and the
    /// keys (Enter, Esc) go to the wizard.
    fn show_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        window.close_sheet(cx);
        window.close_all_dialogs(cx);
        self.acct.open = true;
        if let Some(o) = &self.acct.onboarding {
            window.focus(&o.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// The wizard on its sign-in step (a fresh one, unless one is open).
    pub(super) fn open_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.acct.open && self.acct.onboarding.is_some() {
            return;
        }
        let o = cx.new(|cx| pages::onboarding::Onboarding::signing_in(window, cx));
        self.watch_onboarding(&o, window, cx);
        self.acct.onboarding = Some(o);
        self.show_onboarding(window, cx);
    }

    /// Open the sign-in step from a context without the window.
    pub(super) fn open_sign_in_later(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = cx.windows().first().copied() {
            let view = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = view.update(cx, |this, cx| this.open_sign_in(window, cx));
                });
            });
        }
    }

    fn watch_onboarding(&mut self, o: &Entity<pages::onboarding::Onboarding>, window: &mut Window, cx: &mut Context<Self>) {
        self._subs.push(cx.subscribe_in(o, window, |this, _, ev: &OnboardingEvent, window, cx| match ev {
            OnboardingEvent::Sync => this.sync_now(&SyncNow, cx),
            OnboardingEvent::SignedIn => this.after_sign_in(cx),
            OnboardingEvent::OpenUpload => {
                this.acct.open = false;
                window.focus(&this.focus, cx);
                this.go(Page::Upload, window, cx);
            }
            OnboardingEvent::Close => {
                // closed before an added account signed in: it goes, and the
                // app restarts on the account in use before
                #[cfg(not(target_family = "wasm"))]
                if finance_core::profiles::cancel_unfinished().unwrap_or(false) {
                    super::accounts_menu::relaunch();
                }
                this.acct.vault.update(cx, |v, cx| v.refresh(cx));
                this.acct.open = false;
                this.acct.onboarding = None;
                window.focus(&this.focus, cx);
                cx.notify();
            }
        }));
    }

    /// Signed in (the wizard, or Settings): fetch the data, once.
    pub(super) fn after_sign_in(&mut self, cx: &mut Context<Self>) {
        signin::user_action();
        self.acct.account.update(cx, |a, cx| a.refresh_cx(cx));
        #[cfg(target_family = "wasm")]
        self.refetch_web(cx);
        #[cfg(not(target_family = "wasm"))]
        self.sync_now(&SyncNow, cx);
    }

    /// The browser: the data again, in the page (no reload: a reload while
    /// turned away is what used to loop). At most once per user action.
    #[cfg(target_family = "wasm")]
    pub(super) fn refetch_web(&mut self, cx: &mut Context<Self>) {
        use finance_core::client::{self, Request};
        if !signin::may_request() {
            return;
        }
        self.sync = super::SyncState::Running;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let l = client::send(Request::new("GET", "/api/ledger.json")).await;
            let b = client::send(Request::new("GET", "/api/export.json")).await;
            let res = match (l, b) {
                (Ok(l), Ok(b)) if l.ok() && b.ok() => {
                    signin::note_status(200);
                    let a = client::send(Request::new("GET", "/api/profile/avatar")).await;
                    let pic = a.ok().filter(|a| a.status == 200 && a.body.starts_with(b"\x89PNG")).map(|a| a.body);
                    let _ = finance_core::cache::set_avatar(pic.as_deref());
                    finance_core::preload::set(&l.body, &b.body).map_err(|e| format!("{e:#}"))
                }
                (Ok(l), Ok(b)) => {
                    let status = if l.ok() { b.status } else { l.status };
                    signin::note_status(status);
                    Err(if status == 401 { signin::NOT_SIGNED_IN.to_string() } else { format!("HTTP {status}") })
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            };
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(()) => {
                        this.sync = super::SyncState::Idle;
                        this.load_cache(false, cx);
                    }
                    Err(e) => this.sync = super::SyncState::Failed(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Every load: the shared settings, the avatar, and the wizard on an empty server.
    pub(super) fn after_store(&mut self, bundle: &Bundle, cx: &mut Context<Self>) {
        crate::settings_sync::on_bundle(bundle, cx);
        crate::avatar::refresh(bundle.0.get("profile").unwrap_or(&serde_json::Value::Null), cx);
        let caps = Capabilities::of(bundle);
        let has_data = caps.sources.iter().any(|s| s.has_data);
        let configured = caps.sources.iter().any(|s| s.configured && s.kind != "bank" && s.kind != "card" && s.group != "funds" && s.kind != "crypto_wallet");
        let step = bundle.str(&["profile", "onboarding_step"]).map(String::from);
        let run = caps.known && finance_core::onboarding::should_run(has_data, configured, step.as_deref());
        if run && !std::mem::replace(&mut self.acct.auto_done, true) {
            // deferred, window and all: the first load runs while the window
            // is still being built (the browser's preloaded data), when
            // cx.windows() doesn't have it yet
            let view = cx.entity().downgrade();
            cx.defer(move |cx| {
                if let Some(handle) = cx.windows().first().copied() {
                    let _ = handle.update(cx, |_, window, cx| {
                        let _ = view.update(cx, |this, cx| this.open_onboarding(false, window, cx));
                    });
                }
            });
        }
        self.acct.account.update(cx, |_, cx| cx.notify());
    }

    /// The wizard, when it's open: the app draws it alone, over the whole window.
    pub(super) fn onboarding_el(&self) -> Option<AnyElement> {
        self.acct.open.then(|| self.acct.onboarding.clone().map(|o| o.into_any_element())).flatten()
    }
}
