//! Signing in, without UI (both builds): whether the server turned this
//! device away, the loop guard that keeps a turned-away app from asking
//! again and again, the code and error helpers the sign-in screen uses, and
//! the resume key.
//!
//! The resume key: a random string this device keeps for its server. A
//! sign-out sends it (`POST /api/logout {resume_key}`) and gets back the
//! code to jump back in; every sign-in sends it too (`/api/pair`,
//! `/api/login`), and the server takes that code only with it, once. So the
//! code is useless anywhere else. The desktop keeps it with its tokens
//! (auth.rs), the browser in its origin's localStorage (the web entry
//! point's hooks, `set_resume_hooks`). *Sign out for good* sends none.
//!
//! The loop guard: after a 401 on the data, the app may fetch the data
//! again only once per user action (Sign in, Sync now, Try again). Timers
//! and page loads don't count. The web app once looped on this: a 401
//! opened /login while the app's own "the data is stale" sync reloaded the
//! page, which fetched, got a 401, and so on, several times a second. Now
//! a 401 never navigates and never reloads; it shows the sign-in screen.

use std::sync::Mutex;

/// The guard's state. One per process ([`note_status`] and friends); a
/// value of its own in the tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Guard {
    /// the server answered 401 and nothing has succeeded since
    blocked: bool,
    /// requests allowed while blocked: one per user action
    budget: u32,
}

impl Guard {
    /// What the server answered a data request.
    pub fn note(&mut self, status: u16) {
        match status {
            401 => {
                self.blocked = true;
                self.budget = 0;
            }
            200..=299 => *self = Guard::default(),
            _ => {}
        }
    }

    /// The owner asked (a button, a key): one more try.
    pub fn user_action(&mut self) {
        self.budget = 1;
    }

    /// May the data be requested now? Spends the try when blocked.
    pub fn may_request(&mut self) -> bool {
        if !self.blocked {
            return true;
        }
        if self.budget > 0 {
            self.budget -= 1;
            return true;
        }
        false
    }

    pub fn blocked(&self) -> bool {
        self.blocked
    }
}

static GUARD: Mutex<Guard> = Mutex::new(Guard { blocked: false, budget: 0 });

fn with<R>(f: impl FnOnce(&mut Guard) -> R) -> R {
    f(&mut GUARD.lock().unwrap_or_else(|e| e.into_inner()))
}

pub fn note_status(status: u16) {
    with(|g| g.note(status));
}

pub fn user_action() {
    with(Guard::user_action);
}

pub fn may_request() -> bool {
    with(Guard::may_request)
}

/// The server turned this device away: show the sign-in screen.
pub fn needed() -> bool {
    with(|g| g.blocked())
}

/// Signed in now (a code or a new account worked): the data may be fetched.
pub fn signed_in() {
    note_status(200);
}

/// The browser's resume key store: (the key kept, if any; the key kept,
/// made now when there is none).
#[cfg(not(feature = "native"))]
type ResumeHooks = (fn() -> Option<String>, fn() -> Option<String>);
#[cfg(not(feature = "native"))]
static RESUME: std::sync::OnceLock<ResumeHooks> = std::sync::OnceLock::new();

/// The web entry point's localStorage for the resume key.
#[cfg(not(feature = "native"))]
pub fn set_resume_hooks(held: fn() -> Option<String>, ensure: fn() -> Option<String>) {
    let _ = RESUME.set((held, ensure));
}

/// The resume key this device keeps for its server, if any: sent with a
/// sign-in.
pub fn resume_key() -> Option<String> {
    #[cfg(feature = "native")]
    {
        crate::auth::resume_key(&crate::cache::server_url())
    }
    #[cfg(not(feature = "native"))]
    {
        RESUME.get().and_then(|h| (h.0)())
    }
}

/// [`resume_key`], made now when there is none: sent with a sign-out that
/// wants a code to jump back in. None when it can't be kept.
pub fn ensure_resume_key() -> Option<String> {
    #[cfg(feature = "native")]
    {
        crate::auth::ensure_resume_key(&crate::cache::server_url()).ok()
    }
    #[cfg(not(feature = "native"))]
    {
        RESUME.get().and_then(|h| (h.1)())
    }
}

/// What a sync says when it doesn't ask (the server turned us away).
pub const NOT_SIGNED_IN: &str = "the server wants this device signed in: Settings, Account, or the sign-in screen";

pub const CODE_LEN: usize = 8;

/// "abcd-efgh ", "ABCDEFGH" -> "ABCDEFGH" (what the server compares).
pub fn normalize_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// The letters and digits a code is made of (the server's alphabet: no
/// 0/O, 1/I/L, so nothing reads as something else).
pub const CODE_ALPHABET: &str = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// What a code box keeps of what was typed or pasted: the code's own
/// characters, upper case, at most [`CODE_LEN`] ("abcd-efgh" -> "ABCDEFGH";
/// a character no code has, like O or 1, is dropped).
pub fn clean_code(typed: &str) -> String {
    typed.chars().map(|c| c.to_ascii_uppercase()).filter(|c| CODE_ALPHABET.contains(*c)).take(CODE_LEN).collect()
}

/// The code in pasted text, cleaned: "abcd-efgh", " ABCD EFGH " and a
/// half "abcd" all go in. Anything else (a sentence, a link) is refused
/// rather than cut to eight characters that would spend one of the
/// server's tries.
pub fn pasted_code(text: &str) -> Result<String, &'static str> {
    let bare: String = text.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    let code = clean_code(&bare);
    if code.is_empty() || bare.chars().count() > CODE_LEN || !bare.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(NO_CODE_TO_PASTE);
    }
    Ok(code)
}

/// The Paste button found something else on the clipboard.
pub const NO_CODE_TO_PASTE: &str = "The clipboard holds no code: copy it again, or type it.";

/// The Paste button couldn't read the clipboard (the browser said no).
pub const CLIPBOARD_UNREADABLE: &str = "Couldn't read the clipboard: paste with Ctrl+V or type it.";

/// "abcdefgh" -> "ABCD-EFGH", the way codes are shown.
pub fn format_code(code: &str) -> String {
    let n = normalize_code(code);
    if n.len() > 4 { format!("{}-{}", &n[..4], &n[4..]) } else { n }
}

/// Why a typed code can't be sent yet; None = it can. Checked before
/// sending, so a typo doesn't spend one of the server's ten tries.
pub fn code_problem(code: &str) -> Option<&'static str> {
    let n = normalize_code(code);
    if n.is_empty() {
        Some("Enter the code first.")
    } else if n.len() != CODE_LEN {
        Some("A code has 8 letters and digits, like ABCD-EFGH.")
    } else {
        None
    }
}

/// One code, one request. The sign-in cards send a code by themselves once
/// it is whole, and also on Enter or the Sign in button; the server counts
/// every wrong code, ten lock the client out, so a code goes out once and
/// only again when the owner asks. State only: the page holds one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CodeSubmit {
    /// a request is out, its answer not in yet
    in_flight: bool,
    /// the last code sent, whatever the answer was
    last: Option<String>,
}

impl CodeSubmit {
    /// A request is out: the button says so, and nothing else goes out.
    pub fn pending(&self) -> bool {
        self.in_flight
    }

    fn begin(&mut self, code: &str) -> bool {
        self.in_flight = true;
        self.last = Some(code.to_string());
        true
    }

    /// The slots hold `code` (cleaned): send it by itself? Only a whole
    /// code that is not the last one sent, with no request out. A refused
    /// code is not sent again by an edit that lands on it, or by a paste of
    /// it again: that waits for the owner (Sign in).
    pub fn auto(&mut self, code: &str) -> bool {
        code.chars().count() == CODE_LEN && !self.in_flight && self.last.as_deref() != Some(code) && self.begin(code)
    }

    /// The owner pressed Enter or Sign in with `code` in the slots: sends it
    /// when it is sendable and no request is out, even a code already sent
    /// and refused (they ask again knowingly). A key held down repeats
    /// (`held`) and does not.
    pub fn asked(&mut self, code: &str, held: bool) -> bool {
        !held && !self.in_flight && code_problem(code).is_none() && self.begin(&normalize_code(code))
    }

    /// The answer came, whatever it was.
    pub fn answered(&mut self) {
        self.in_flight = false;
    }
}

/// How a new account is made on this server (whoami's `signup`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signup {
    /// no users yet: the first account becomes the admin
    First,
    /// FINANCE_ALLOW_SIGNUP: no invite needed
    Open,
    Invite,
}

impl Signup {
    pub fn parse(s: &str) -> Signup {
        match s {
            "first" => Signup::First,
            "open" => Signup::Open,
            _ => Signup::Invite,
        }
    }
}

/// The words for a refused sign-in: the server's own message when it sent
/// one, else one for its status.
pub fn refusal(status: u16, body: &serde_json::Value) -> String {
    if let Some(e) = body.get("error").and_then(|e| e.as_str()).filter(|e| !e.is_empty()) {
        return e.to_string();
    }
    match status {
        429 => "Too many wrong codes: wait a few minutes and try again.".into(),
        403 => "The server refused: ask its admin.".into(),
        0 => "The server didn't answer.".into(),
        s => format!("The server answered HTTP {s}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_boxes_keep_only_code_characters() {
        assert_eq!(clean_code("abcd-efgh"), "ABCDEFGH");
        assert_eq!(clean_code(" ab cd-ef gh 23"), "ABCDEFGH");
        assert_eq!(clean_code("O1IL0"), "", "characters no code has");
        assert_eq!(clean_code("K7M2"), "K7M2");
    }

    #[test]
    fn a_pasted_code_goes_in_whole_and_anything_else_is_refused() {
        assert_eq!(pasted_code("abcd-efgh"), Ok("ABCDEFGH".into()));
        assert_eq!(pasted_code(" ABCD EFGH \n"), Ok("ABCDEFGH".into()));
        assert_eq!(pasted_code("\tk7m2-\r\n"), Ok("K7M2".into()), "half a code fills half the slots");
        assert_eq!(pasted_code(""), Err(NO_CODE_TO_PASTE));
        assert_eq!(pasted_code("  - "), Err(NO_CODE_TO_PASTE));
        assert_eq!(pasted_code("Your code: ABCD-EFGH"), Err(NO_CODE_TO_PASTE), "not cut to its first eight letters");
        assert_eq!(pasted_code("https://example.com/x"), Err(NO_CODE_TO_PASTE));
        assert_eq!(pasted_code("ABCD-EFGH-JKMN"), Err(NO_CODE_TO_PASTE), "too long to be one code");
    }

    #[test]
    fn a_turned_away_app_asks_once_per_user_action() {
        let mut g = Guard::default();
        assert!(g.may_request() && g.may_request(), "not blocked: ask freely");
        g.note(401);
        assert!(g.blocked());
        // timers, page loads, re-renders: nothing gets through
        for _ in 0..100 {
            assert!(!g.may_request());
        }
        g.user_action();
        assert!(g.may_request(), "the click gets one try");
        assert!(!g.may_request(), "and only one");
        g.user_action();
        g.user_action();
        assert!(g.may_request());
        assert!(!g.may_request(), "two clicks before a request still mean one");
        // a server error doesn't unblock; a success does
        g.note(500);
        assert!(g.blocked());
        g.note(200);
        assert!(!g.blocked() && g.may_request() && g.may_request());
    }

    #[test]
    fn a_code_is_sent_once() {
        let mut s = CodeSubmit::default();
        assert!(!s.auto("ABCD"), "half a code waits");
        assert!(s.auto("ABCDEFGH") && s.pending());
        // pastes, edits and Enter while the request is out: nothing
        assert!(!s.auto("ABCDEFGH") && !s.auto("JKMNPQRS") && !s.asked("ABCD-EFGH", false));
        s.answered();
        assert!(!s.pending());
        // refused: the same code is not sent again by itself ...
        assert!(!s.auto("ABCDEFGH"));
        // ... a different one is ...
        assert!(s.auto("JKMNPQRS"));
        s.answered();
        // ... and Sign in sends it again, once; a held Enter does not
        assert!(!s.asked("JKMNPQRS", true));
        assert!(s.asked("jkmn-pqrs", false));
        assert!(!s.asked("JKMNPQRS", false) && !s.auto("JKMNPQRS"));
        s.answered();
        assert!(!s.asked("JKMN", false), "an incomplete code is the page's note, not a request");
    }

    #[test]
    fn codes() {
        assert_eq!(normalize_code(" abcd-efgh "), "ABCDEFGH");
        assert_eq!(format_code("abcdefgh"), "ABCD-EFGH");
        assert_eq!(format_code("ab"), "AB");
        assert_eq!(code_problem(""), Some("Enter the code first."));
        assert!(code_problem("ABCD-EFG").is_some());
        assert_eq!(code_problem("abcd efgh"), None);
        assert_eq!(Signup::parse("first"), Signup::First);
        assert_eq!(Signup::parse("whatever"), Signup::Invite);
        assert_eq!(refusal(400, &serde_json::json!({"error": "That code has expired: ask for a new one."})), "That code has expired: ask for a new one.");
        assert!(refusal(429, &serde_json::Value::Null).contains("a few minutes"));
    }
}
