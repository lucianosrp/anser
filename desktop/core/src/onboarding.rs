//! The first-run wizard as a state machine, with no UI in it (it builds for
//! wasm too; app/src/pages/onboarding.rs draws it on both).
//!
//! Steps: a welcome, where the data lives (desktop only: run a server locally, or
//! connect to one), sign in (a pairing code, or a new account), your name,
//! currency and home, your picture, your money (a checklist from the source
//! catalog), API sources (one card per chosen source: test then save its
//! key), file sources (export instructions, then upload), protect this
//! computer's copy (desktop only), the first sync, done. The app draws each as one small card (at most three fields), so
//! the count it shows ([`Wizard::progress`]) is of cards, not steps.
//!
//! [`Wizard::next`] and [`Wizard::back`] skip the steps that don't apply: no
//! "where" in the browser (the page is already on its server), no sign-in
//! when the server already knows this device (or wants nothing), no API
//! step when nothing chosen needs a key, no file step when nothing chosen
//! is a file export. [`should_run`] decides when the app opens it by itself;
//! [`Wizard::signing_in`] is the wizard the app opens when the server turns
//! it away (a 401): the sign-in step is then its first screen.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// the first card: the promise, "Get started" or "Just try it"
    Welcome,
    Where,
    SignIn,
    /// the name (the key stays "you", the step that also held the rest)
    You,
    /// base currency, home country and city
    Home,
    Picture,
    Money,
    ApiSources,
    FileSources,
    /// encrypt this computer's copy (desktop only), before any data lands
    Protect,
    FirstSync,
    Done,
}

impl Step {
    pub const ALL: [Step; 12] = [
        Step::Welcome, Step::Where, Step::SignIn, Step::You, Step::Home, Step::Picture, Step::Money, Step::ApiSources, Step::FileSources,
        Step::Protect, Step::FirstSync, Step::Done,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome",
            Step::Where => "Where your data lives",
            Step::SignIn => "Sign in",
            Step::You => "What's your name?",
            Step::Home => "Where you are",
            Step::Picture => "Your picture",
            Step::Money => "Where is your money?",
            Step::ApiSources => "Connect accounts",
            Step::FileSources => "Bank exports",
            Step::Protect => "Protect this computer",
            Step::FirstSync => "First sync",
            Step::Done => "You're all set",
        }
    }

    /// The profile's `onboarding_step` value.
    pub fn key(self) -> &'static str {
        match self {
            Step::Welcome => "welcome",
            Step::Where => "where",
            Step::SignIn => "sign_in",
            Step::You => "you",
            Step::Home => "home",
            Step::Picture => "picture",
            Step::Money => "money",
            Step::ApiSources => "api_sources",
            Step::FileSources => "file_sources",
            Step::Protect => "protect",
            Step::FirstSync => "first_sync",
            Step::Done => "done",
        }
    }

    pub fn from_key(k: &str) -> Option<Step> {
        Step::ALL.into_iter().find(|s| s.key() == k)
    }

    /// Can be left without doing it (the card shows Skip).
    pub fn skippable(self) -> bool {
        matches!(self, Step::Picture | Step::ApiSources | Step::FileSources | Step::Protect | Step::FirstSync)
    }
}

/// Desktop: a server on this machine, or one elsewhere (a home server, say).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Place {
    Local,
    Remote,
}

/// The headings the "your money" checklist is grouped under, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Banks,
    Investments,
    Crypto,
}

impl Group {
    pub const ALL: [Group; 3] = [Group::Banks, Group::Investments, Group::Crypto];

    pub fn title(self) -> &'static str {
        match self {
            Group::Banks => "Banks",
            Group::Investments => "Investments",
            Group::Crypto => "Crypto",
        }
    }

    /// An API source's catalog `group` (crypto, broker).
    pub fn of_api(group: &str) -> Group {
        if group == "crypto" { Group::Crypto } else { Group::Investments }
    }

    /// A file source's catalog `kind` (bank, card, fund_units, crypto_wallet).
    pub fn of_file(kind: &str) -> Group {
        match kind {
            "fund_units" => Group::Investments,
            "crypto_wallet" => Group::Crypto,
            _ => Group::Banks,
        }
    }
}

/// One entry of the "your money" checklist (from /api/sources/catalog).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub key: String,
    pub label: String,
    /// an API source (keys), else a file source (exports)
    pub api: bool,
    pub group: Group,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub name: String,
    pub base_currency: String,
    pub home_country: String,
    pub home_city: String,
}

impl Default for Profile {
    fn default() -> Self {
        Profile { name: String::new(), base_currency: crate::locale::FALLBACK_CURRENCY.into(), home_country: String::new(), home_city: String::new() }
    }
}

impl Profile {
    /// Why it can't be saved, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        let c = self.base_currency.trim();
        if c.len() != 3 || !c.chars().all(|ch| ch.is_ascii_alphabetic()) {
            return Some("The base currency is a 3-letter code, like HKD, EUR or USD.");
        }
        let cc = self.home_country.trim();
        if !cc.is_empty() && (cc.len() != 2 || !cc.chars().all(|ch| ch.is_ascii_alphabetic())) {
            return Some("The home country is a 2-letter code, like US or DE.");
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Wizard {
    pub step: Step,
    /// the welcome card applies (not when the server turned this device away)
    pub welcome: bool,
    /// the browser: no "where" step
    pub web: bool,
    pub place: Option<Place>,
    /// the chosen server answered (desktop; the browser is on its server)
    pub connected: bool,
    /// the sign-in step applies: the server doesn't know this device
    pub sign_in: bool,
    /// paired with the chosen server (or it needs no pairing)
    pub paired: bool,
    pub profile: Profile,
    pub catalog: Vec<Choice>,
    pub chosen: BTreeSet<String>,
    /// API sources saved (tested) so far
    pub saved: BTreeSet<String>,
    /// which chosen API source's card is showing (ApiSources has one each)
    pub api_ix: usize,
    pub synced: bool,
}

impl Wizard {
    pub fn new(web: bool) -> Self {
        Wizard {
            step: Step::Welcome,
            welcome: true,
            web,
            place: None,
            connected: web,
            sign_in: false,
            paired: false,
            profile: Profile::default(),
            catalog: vec![],
            chosen: BTreeSet::new(),
            saved: BTreeSet::new(),
            api_ix: 0,
            synced: false,
        }
    }

    /// The wizard for a server that turned this device away: it opens on
    /// the sign-in step (the server is known: the browser's own, or the
    /// desktop's configured one).
    pub fn signing_in(web: bool) -> Self {
        let mut w = Wizard::new(web);
        w.step = Step::SignIn;
        // the browser's server is fixed by its URL: nothing behind sign-in; the
        // desktop keeps Where and the welcome behind it (Back, to pick another server)
        w.welcome = !web;
        w.sign_in = true;
        w.connected = true;
        if !web {
            w.place = Some(Place::Remote);
        }
        w
    }

    /// What the server said about this device (whoami): `known` = paired,
    /// or the server wants no pairing.
    pub fn server_answered(&mut self, known: bool) {
        self.connected = true;
        self.paired = known;
        // once shown, the step stays in the count until the wizard closes
        self.sign_in = self.sign_in || !known;
    }

    fn chosen_api(&self) -> impl Iterator<Item = &Choice> {
        self.catalog.iter().filter(|c| c.api && self.chosen.contains(&c.key))
    }

    fn chosen_files(&self) -> impl Iterator<Item = &Choice> {
        self.catalog.iter().filter(|c| !c.api && self.chosen.contains(&c.key))
    }

    pub fn api_to_set_up(&self) -> Vec<&Choice> {
        self.chosen_api().collect()
    }

    /// The API source whose card is showing.
    pub fn current_api(&self) -> Option<&Choice> {
        self.chosen_api().nth(self.api_ix)
    }

    pub fn files_to_export(&self) -> Vec<&Choice> {
        self.chosen_files().collect()
    }

    pub fn applies(&self, s: Step) -> bool {
        match s {
            Step::Welcome => self.welcome,
            Step::Where => !self.web,
            Step::Protect => !self.web,
            Step::SignIn => self.sign_in,
            Step::ApiSources => self.chosen_api().next().is_some(),
            Step::FileSources => self.chosen_files().next().is_some(),
            _ => true,
        }
    }

    /// Can the current step be left forward? None = yes, else why not.
    pub fn blocker(&self) -> Option<&'static str> {
        match self.step {
            Step::Where if self.place.is_none() => Some("Choose where the data lives."),
            Step::Where if !self.connected => Some("Connect to the server first."),
            Step::SignIn if !self.paired => Some("Sign in first: enter a pairing code, or create an account."),
            Step::Home => self.profile.problem(),
            _ => None,
        }
    }

    pub fn next(&mut self) -> Result<Step, &'static str> {
        if let Some(why) = self.blocker() {
            return Err(why);
        }
        if self.step == Step::ApiSources && self.api_ix + 1 < self.chosen_api().count() {
            self.api_ix += 1;
            return Ok(self.step);
        }
        let i = Step::ALL.iter().position(|s| *s == self.step).unwrap_or(0);
        if let Some(s) = Step::ALL[i + 1..].iter().copied().find(|s| self.applies(*s)) {
            self.step = s;
            self.api_ix = 0;
        }
        Ok(self.step)
    }

    pub fn back(&mut self) -> Step {
        if self.step == Step::ApiSources && self.api_ix > 0 {
            self.api_ix -= 1;
            return self.step;
        }
        let i = Step::ALL.iter().position(|s| *s == self.step).unwrap_or(0);
        if let Some(s) = Step::ALL[..i].iter().rev().copied().find(|s| self.applies(*s)) {
            self.step = s;
            // back into the keys: the last source's card
            self.api_ix = if s == Step::ApiSources { self.chosen_api().count().saturating_sub(1) } else { 0 };
        }
        self.step
    }

    /// Is there a card before this one?
    pub fn back_possible(&self) -> bool {
        let i = Step::ALL.iter().position(|s| *s == self.step).unwrap_or(0);
        (self.step == Step::ApiSources && self.api_ix > 0) || Step::ALL[..i].iter().any(|s| self.applies(*s))
    }

    pub fn toggle(&mut self, key: &str) {
        if !self.chosen.remove(key) {
            self.chosen.insert(key.to_string());
        }
        self.api_ix = 0;
    }

    /// Every card that applies: (step, which API source) in order. The
    /// welcome isn't counted: it comes before the count starts.
    fn cards(&self) -> Vec<(Step, usize)> {
        let n_api = self.chosen_api().count();
        Step::ALL.into_iter().filter(|s| *s != Step::Welcome && self.applies(*s))
            .flat_map(|s| (0..if s == Step::ApiSources { n_api } else { 1 }).map(move |i| (s, i)))
            .collect()
    }

    /// (number, of) for "2 of 6", counting the cards that apply (0 on the welcome).
    pub fn progress(&self) -> (usize, usize) {
        let cards = self.cards();
        let at = (self.step, if self.step == Step::ApiSources { self.api_ix } else { 0 });
        let n = if self.step == Step::Welcome { 0 } else { cards.iter().position(|c| *c == at).map(|i| i + 1).unwrap_or(1) };
        (n, cards.len())
    }

    /// The name of each card that applies, for the progress dots' tooltips.
    pub fn card_names(&self) -> Vec<String> {
        let api: Vec<&str> = self.chosen_api().map(|c| c.label.as_str()).collect();
        self.cards().into_iter()
            .map(|(s, i)| match (s, api.get(i)) {
                (Step::ApiSources, Some(l)) => format!("Connect {l}"),
                _ => s.title().to_string(),
            })
            .collect()
    }
}

/// How the Protect card seals this computer's copy: the switch (on by
/// default), "use a passphrase instead", and whether the OS keyring works
/// (None: still being probed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protect {
    /// a random key in the OS keyring: nothing to type
    Keyring,
    /// a passphrase asked for at every start (chosen, or no keyring here)
    Passphrase,
    /// left plain (the switch is off)
    Off,
    /// the keyring probe hasn't answered yet
    Checking,
}

/// Whether the Protect card makes the nightly-backup key: the admin, with no
/// key on this computer, and only once the server is known to have no
/// recipient (one set from another computer must not be replaced; an answer
/// still pending, or refused, is a no).
pub fn offer_backup_key(admin: bool, key_missing: bool, server_has_recipient: Option<bool>) -> bool {
    admin && key_missing && server_has_recipient == Some(false)
}

pub fn protect_plan(on: bool, passphrase: bool, keyring: Option<bool>) -> Protect {
    match (on, passphrase, keyring) {
        (false, _, _) => Protect::Off,
        (_, true, _) | (_, _, Some(false)) => Protect::Passphrase,
        (_, _, Some(true)) => Protect::Keyring,
        (_, _, None) => Protect::Checking,
    }
}

/// Open the wizard by itself? Only on an empty server: no data, no
/// configured source, and onboarding not finished (or never started).
pub fn should_run(has_data: bool, any_configured: bool, onboarding_step: Option<&str>) -> bool {
    !has_data && !any_configured && onboarding_step != Some("done")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<Choice> {
        vec![
            Choice { key: "binance".into(), label: "Binance".into(), api: true, group: Group::Crypto },
            Choice { key: "okx".into(), label: "OKX".into(), api: true, group: Group::Crypto },
            Choice { key: "n26".into(), label: "N26".into(), api: false, group: Group::of_file("bank") },
        ]
    }

    #[test]
    fn a_new_profile_assumes_no_country() {
        let p = Profile::default();
        assert_eq!((p.base_currency.as_str(), p.home_country.as_str()), ("USD", ""));
        assert_eq!(p.problem(), None);
    }

    #[test]
    fn desktop_path_skips_what_doesnt_apply() {
        let mut w = Wizard::new(false);
        w.catalog = catalog();
        assert_eq!(w.step, Step::Welcome);
        assert_eq!(w.progress(), (0, 8), "the welcome isn't counted");
        assert!(!w.back_possible());
        assert_eq!(w.next(), Ok(Step::Where));
        assert!(w.back_possible());
        assert!(w.next().is_err(), "choose a place first");
        w.place = Some(Place::Remote);
        assert_eq!(w.next(), Err("Connect to the server first."));
        // a server that already knows this device: no sign-in step
        w.server_answered(true);
        assert_eq!(w.next(), Ok(Step::You));
        assert_eq!(w.next(), Ok(Step::Home), "the name is optional");
        w.profile.base_currency = "dollars".into();
        assert!(w.next().is_err());
        w.profile.base_currency = "EUR".into();
        assert_eq!(w.next(), Ok(Step::Picture));
        assert!(Step::Picture.skippable() && !Step::Home.skippable());
        assert_eq!(w.next(), Ok(Step::Money));
        // nothing chosen: straight to protecting the copy, then the first sync
        assert_eq!(w.next(), Ok(Step::Protect));
        assert!(Step::Protect.skippable());
        assert_eq!(w.next(), Ok(Step::FirstSync));
        assert_eq!(w.back(), Step::Protect);
        assert_eq!(w.back(), Step::Money);
        w.toggle("n26");
        assert_eq!(w.next(), Ok(Step::FileSources));
        assert_eq!(w.back(), Step::Money);
        w.toggle("binance");
        assert_eq!(w.next(), Ok(Step::ApiSources));
        // Where, You, Home, Picture, Money, Binance, Files, Protect, First sync, Done
        assert_eq!(w.progress(), (6, 10));
        assert_eq!(w.card_names()[5], "Connect Binance");
        assert_eq!(Step::from_key("sign_in"), Some(Step::SignIn));
        assert_eq!(w.next(), Ok(Step::FileSources));
        assert_eq!(w.next(), Ok(Step::Protect));
        assert_eq!(w.next(), Ok(Step::FirstSync));
        assert_eq!(w.next(), Ok(Step::Done));
        assert_eq!(w.next(), Ok(Step::Done), "done stays done");
    }

    #[test]
    fn one_card_per_api_source() {
        let mut w = Wizard::new(true);
        w.catalog = catalog();
        w.step = Step::Money;
        w.toggle("binance");
        w.toggle("okx");
        assert_eq!(w.next(), Ok(Step::ApiSources));
        assert_eq!(w.current_api().map(|c| c.key.as_str()), Some("binance"));
        assert_eq!(w.progress(), (5, 8), "You, Home, Picture, Money, Binance, OKX, First sync, Done");
        assert_eq!(w.next(), Ok(Step::ApiSources));
        assert_eq!(w.current_api().map(|c| c.key.as_str()), Some("okx"));
        assert_eq!(w.progress(), (6, 8));
        assert_eq!(w.back(), Step::ApiSources);
        assert_eq!(w.current_api().map(|c| c.key.as_str()), Some("binance"));
        assert!(w.back_possible());
        assert_eq!(w.back(), Step::Money);
        assert_eq!(w.next(), Ok(Step::ApiSources));
        w.next().unwrap();
        assert_eq!(w.next(), Ok(Step::FirstSync), "no file source chosen");
        // back from after the keys lands on the last source's card
        assert_eq!(w.back(), Step::ApiSources);
        assert_eq!(w.current_api().map(|c| c.key.as_str()), Some("okx"));
    }

    #[test]
    fn web_has_no_where_step() {
        let mut w = Wizard::new(true);
        assert_eq!(w.step, Step::Welcome);
        assert_eq!(w.next(), Ok(Step::You), "no where step, no protect step");
        assert_eq!(w.back(), Step::Welcome);
        assert_eq!(w.next(), Ok(Step::You));
        // You, Home, Picture, Money, First sync, Done
        assert_eq!(w.progress(), (1, 6));
        assert_eq!(w.next(), Ok(Step::Home));
        w.profile.home_country = "Hong Kong".into();
        assert!(w.next().is_err(), "a 2-letter country");
        w.profile.home_country = "hk".into();
        assert_eq!(w.next(), Ok(Step::Picture));
        assert_eq!(Step::from_key("home"), Some(Step::Home));
        assert_eq!(Step::from_key("picture"), Some(Step::Picture));
    }

    #[test]
    fn a_new_device_signs_in_after_connecting() {
        let mut w = Wizard::new(false);
        w.step = Step::Where;
        w.place = Some(Place::Remote);
        w.server_answered(false);
        assert_eq!(w.next(), Ok(Step::SignIn));
        assert_eq!(w.next(), Err("Sign in first: enter a pairing code, or create an account."));
        assert_eq!(w.progress(), (2, 9));
        w.server_answered(true);
        assert_eq!(w.progress(), (2, 9), "signed in: the step still counts");
        assert_eq!(w.next(), Ok(Step::You));
        assert_eq!(w.back(), Step::SignIn);
    }

    #[test]
    fn a_fresh_device_on_an_unclaimed_server_signs_in_after_the_welcome() {
        // the browser (no where step): the server's answer arrives while the welcome shows
        let mut w = Wizard::new(true);
        assert_eq!(w.step, Step::Welcome);
        assert!(!w.paired);
        w.server_answered(false);
        assert_eq!(w.next(), Ok(Step::SignIn));
        assert!(w.next().is_err(), "no way past sign-in before it's done");
        w.server_answered(true);
        assert_eq!(w.next(), Ok(Step::You));
    }

    #[test]
    fn back_from_the_first_card_returns_to_the_welcome() {
        for web in [true, false] {
            let mut w = Wizard::new(web);
            w.place = Some(Place::Local);
            w.server_answered(true);
            let first = w.next().unwrap();
            assert_eq!(first, if web { Step::You } else { Step::Where });
            assert!(w.back_possible());
            assert_eq!(w.back(), Step::Welcome);
            assert!(!w.back_possible(), "nothing before the welcome");
            assert_eq!(w.next(), Ok(first), "and forward again");
        }
        let mut w = Wizard::signing_in(false);
        assert_eq!(w.step, Step::SignIn);
        assert_eq!(w.back(), Step::Where);
        assert_eq!(w.back(), Step::Welcome);
        assert!(!w.back_possible());
        assert!(!Wizard::signing_in(true).back_possible(), "the browser: no Back before sign-in");
    }

    #[test]
    fn turned_away_opens_on_sign_in() {
        for web in [true, false] {
            let mut w = Wizard::signing_in(web);
            assert_eq!(w.step, Step::SignIn);
            assert!(w.next().is_err());
            assert_eq!(w.progress().0, if web { 1 } else { 2 }, "the browser's first screen");
            w.server_answered(true);
            assert_eq!(w.next(), Ok(Step::You));
        }
        // the browser: nothing before it
        let w = Wizard::signing_in(true);
        assert!(!w.back_possible(), "no welcome card behind a turned-away sign-in");
    }

    #[test]
    fn sources_group_under_banks_investments_crypto() {
        assert_eq!(Group::ALL.map(Group::title), ["Banks", "Investments", "Crypto"]);
        assert_eq!(Group::of_api("crypto"), Group::Crypto);
        assert_eq!(Group::of_api("broker"), Group::Investments);
        assert_eq!([("bank"), ("card"), ("fund_units"), ("crypto_wallet")].map(Group::of_file),
            [Group::Banks, Group::Banks, Group::Investments, Group::Crypto]);
    }

    #[test]
    fn backup_key_is_offered_only_when_the_server_has_none() {
        assert!(offer_backup_key(true, true, Some(false)));
        assert!(!offer_backup_key(true, true, Some(true)), "another computer's key stays");
        assert!(!offer_backup_key(true, true, None), "never on a guess");
        assert!(!offer_backup_key(true, false, Some(false)));
        assert!(!offer_backup_key(false, true, Some(false)));
    }

    #[test]
    fn protect_is_on_by_default_and_falls_back_to_a_passphrase() {
        // the card opens with the switch on: the keyring, when there is one
        assert_eq!(protect_plan(true, false, Some(true)), Protect::Keyring);
        assert_eq!(protect_plan(true, true, Some(true)), Protect::Passphrase, "asked for");
        // no keyring: never silently skipped, the passphrase keeps it on
        assert_eq!(protect_plan(true, false, Some(false)), Protect::Passphrase);
        assert_eq!(protect_plan(true, false, None), Protect::Checking);
        // the way out: the switch off, continue unencrypted
        assert_eq!(protect_plan(false, false, Some(false)), Protect::Off);
        assert!(Step::Protect.skippable());
    }

    #[test]
    fn runs_only_on_an_empty_server() {
        assert!(should_run(false, false, None));
        assert!(should_run(false, false, Some("money")));
        assert!(!should_run(true, false, None));
        assert!(!should_run(false, true, None));
        assert!(!should_run(false, false, Some("done")));
        assert_eq!(Step::from_key("api_sources"), Some(Step::ApiSources));
    }
}
