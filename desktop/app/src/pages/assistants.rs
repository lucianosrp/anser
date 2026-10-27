//! Settings, AI assistants: switch MCP on, make and revoke keys, and the
//! ready-to-paste setup for each client. Assistants such as Claude or Codex
//! read the finances through the server's /mcp (read-only, tailnet only).
//!
//! * The switch and the key list come from GET /api/mcp; the switch updates
//!   at once and reverts, with a note, if the server says no.
//! * A new key's secret is shown once, in a panel with the snippets for the
//!   chosen client. It lives only in [`Fresh`] (memory): Done, or leaving the
//!   tab, drops it. Nothing writes it anywhere.
//! * No server, or an offline one: the tab offers local mode instead
//!   (desktop only): the command that adds `anser mcp`, a stdio
//!   server over this device's copy, to an assistant (mcp_local.rs).
//! * Dev aid: `FINANCE_DESKTOP_MCP=stub` (the browser: `?mcp=stub`) fills the
//!   tab with made-up keys and no server; `stub-fresh` also opens the panel.

use finance_core::mcp_setup::{self as mcp, AssistantClient, McpKey, McpStatus};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt as _,
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

/// The key shown in the one-time panel.
struct Fresh {
    /// the client whose setup is shown (the panel's own picker)
    client: AssistantClient,
    name: String,
    token: String,
    /// the endpoint, as it was when the key was made
    url: String,
    /// which Copy button was pressed last
    copied: Option<&'static str>,
}

enum Test {
    Idle,
    Running,
    Done(Result<String, String>),
}

pub struct Assistants {
    /// None until the first answer
    status: Option<Result<McpStatus, String>>,
    asked: bool,
    stub: bool,
    note: Option<(bool, String)>,
    confirm_revoke: Option<i64>,
    client: AssistantClient,
    name: Entity<InputState>,
    /// the name was typed by hand: a client change leaves it alone
    name_edited: bool,
    creating: bool,
    fresh: Option<Fresh>,
    test: Test,
    /// local mode: which Copy button was pressed last
    local_copied: Option<&'static str>,
    _subs: Vec<Subscription>,
}

fn web() -> bool {
    cfg!(target_family = "wasm")
}

fn device_name() -> String {
    #[cfg(not(target_family = "wasm"))]
    {
        finance_core::auth::default_device_name()
    }
    #[cfg(target_family = "wasm")]
    {
        String::new()
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// The stub's keys (dev aid): made-up names, relative to now.
fn stub_status() -> McpStatus {
    let (now, day) = (now_ms(), 86_400_000);
    McpStatus {
        enabled: true,
        path: "/mcp".into(),
        keys: vec![
            McpKey { id: 1, name: "Claude Code on laptop".into(), created: now - 12 * day, last_used: Some(now - 2 * day) },
            McpKey { id: 2, name: "Codex on workstation".into(), created: now - 5 * day, last_used: Some(now - 3_600_000 * 3) },
            McpKey { id: 3, name: "Cursor 2026-09-30".into(), created: now - day, last_used: None },
        ],
    }
}

impl Assistants {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let client = AssistantClient::default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Key name")
            .default_value(mcp::default_key_name(client, &device_name(), &today())));
        let subs = vec![cx.subscribe(&name, |this, _, ev: &gpui_kit::component::input::InputEvent, _| {
            if matches!(ev, gpui_kit::component::input::InputEvent::Change) {
                this.name_edited = true;
            }
        })];
        let aid = crate::app::dev_aid("FINANCE_DESKTOP_MCP").unwrap_or_default();
        let mut a = Assistants {
            status: None,
            asked: false,
            stub: aid.starts_with("stub"),
            note: None,
            confirm_revoke: None,
            client,
            name,
            name_edited: false,
            creating: false,
            fresh: None,
            test: Test::Idle,
            local_copied: None,
            _subs: subs,
        };
        if a.stub {
            a.status = Some(Ok(stub_status()));
            a.asked = true;
            if aid.starts_with("stub-fresh") {
                a.fresh = Some(Fresh { client: if aid.ends_with("codex") { AssistantClient::Codex } else { AssistantClient::ClaudeCode },
                    name: "Claude Code on laptop".into(), token: "fin_mcp_3f9a1c7d52e84b06a1d9c0e7b25f8a34".into(),
                    url: mcp::endpoint("https://finance.example.ts.net:8787"), copied: None });
            }
        }
        a
    }

    /// Ask the server again (the tab was opened).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.stub || finance_core::demo::active() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let r = mcp::status().await;
            let _ = this.update(cx, |this, cx| {
                this.status = Some(r);
                cx.notify();
            });
        })
        .detach();
    }

    /// The tab was left: the secret goes with it.
    pub fn forget(&mut self, cx: &mut Context<Self>) {
        self.fresh = None;
        self.test = Test::Idle;
        self.confirm_revoke = None;
        cx.notify();
    }

    fn set_enabled(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(Ok(s)) = &mut self.status else { return };
        s.enabled = on;
        self.note = None;
        if !on {
            self.fresh = None;
        }
        cx.notify();
        if self.stub {
            return;
        }
        cx.spawn(async move |this, cx| {
            let r = mcp::set_enabled(on).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = r {
                    if let Some(Ok(s)) = &mut this.status {
                        s.enabled = !on;
                    }
                    this.note = Some((false, format!("Couldn't switch it {}: {e}", if on { "on" } else { "off" })));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_client(&mut self, c: AssistantClient, window: &mut Window, cx: &mut Context<Self>) {
        self.client = c;
        if !self.name_edited && self.fresh.is_none() {
            let v = mcp::default_key_name(c, &device_name(), &today());
            self.name.update(cx, |s, cx| s.set_value(v, window, cx));
            // set_value reports a change too
            self.name_edited = false;
        }
        cx.notify();
    }

    /// The panel's own picker: the same key, another client's setup.
    fn pick_setup_client(&mut self, c: AssistantClient, cx: &mut Context<Self>) {
        if let Some(f) = &mut self.fresh {
            f.client = c;
            f.copied = None;
        }
        cx.notify();
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() || name.chars().count() > 60 {
            self.note = Some((false, "Name the key, 1 to 60 characters.".into()));
            cx.notify();
            return;
        }
        self.creating = true;
        let client = self.client;
        self.note = None;
        self.test = Test::Idle;
        let url = mcp::endpoint(&if self.stub { "https://finance.example.ts.net:8787".into() } else { finance_core::cache::server_url() });
        if self.stub {
            self.creating = false;
            self.fresh = Some(Fresh { client, name, token: "fin_mcp_3f9a1c7d52e84b06a1d9c0e7b25f8a34".into(), url, copied: None });
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let r = mcp::create_key(&name).await;
            let st = if r.is_ok() { mcp::status().await.ok() } else { None };
            let _ = this.update(cx, |this, cx| {
                this.creating = false;
                match r {
                    Ok(k) => this.fresh = Some(Fresh { client, name: k.name, token: k.token, url, copied: None }),
                    Err(e) => this.note = Some((false, format!("Couldn't make the key: {e}"))),
                }
                if let Some(st) = st {
                    this.status = Some(Ok(st));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn revoke(&mut self, id: i64, cx: &mut Context<Self>) {
        self.confirm_revoke = None;
        self.note = None;
        if self.stub {
            if let Some(Ok(s)) = &mut self.status {
                s.keys.retain(|k| k.id != id);
            }
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let r = mcp::revoke_key(id).await;
            let st = mcp::status().await.ok();
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = r {
                    this.note = Some((false, format!("Couldn't revoke it: {e}")));
                }
                if let Some(st) = st {
                    this.status = Some(Ok(st));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_local(&mut self, which: &'static str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.local_copied = Some(which);
        cx.notify();
    }

    /// Local mode: the setup for `anser mcp` (this executable),
    /// which reads this device's copy. None in the browser, which has none.
    fn local_card(&self, cx: &mut Context<Self>) -> Option<Div> {
        #[cfg(target_family = "wasm")]
        {
            let _ = cx;
            None
        }
        #[cfg(not(target_family = "wasm"))]
        {
            // Linux marks a replaced binary " (deleted)"; the new one is at the same path
            let exe = std::env::current_exe().ok()?.to_string_lossy().trim_end_matches(" (deleted)").to_string();
            let (cmd, json) = (mcp::local_command(&exe), mcp::local_json(&exe));
            let copy_btn = |id: &'static str, label: &'static str, text: String, cx: &mut Context<Self>| {
                let done = self.local_copied == Some(id);
                Button::new(id).small().outline()
                    .icon(if done { IconName::Check } else { IconName::Copy })
                    .label(if done { "Copied" } else { label })
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_local(id, text.clone(), cx)))
            };
            Some(v_flex().gap_2().child(ui::section("Local mode", "this device's copy, no server", cx)).child(ui::card(cx).gap_2()
                .child(ui::caption_wrap("It reads this device's copy, as fresh as the last sync, and needs no key or network. \
                    The assistant starts it itself. Claude Code:", cx))
                .child(code_block("mcp-local-cmd", cmd.clone(), cx))
                .child(h_flex().child(copy_btn("copy-local-cmd", "Copy command", cmd, cx)))
                .child(ui::caption_wrap("Claude Desktop, Cursor and others (their MCP config file):", cx))
                .child(code_block("mcp-local-json", json.clone(), cx))
                .child(h_flex().child(copy_btn("copy-local-json", "Copy config block", json, cx)))))
        }
    }

    fn copy(&mut self, which: &'static str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if let Some(f) = &mut self.fresh {
            f.copied = Some(which);
        }
        cx.notify();
    }

    fn run_test(&mut self, cx: &mut Context<Self>) {
        let Some(f) = &self.fresh else { return };
        if self.stub {
            self.test = Test::Done(Ok("Connected, 8 tools, 42 ms".into()));
            cx.notify();
            return;
        }
        #[cfg(not(target_family = "wasm"))]
        {
            let (url, key) = (f.url.clone(), f.token.clone());
            self.test = Test::Running;
            let job = cx.background_spawn(async move { finance_core::mcp_setup::test_connection(&url, &key) });
            cx.spawn(async move |this, cx| {
                let r = job.await;
                let _ = this.update(cx, |this, cx| {
                    this.test = Test::Done(r);
                    cx.notify();
                });
            })
            .detach();
        }
        #[cfg(target_family = "wasm")]
        let _ = f;
    }
}

fn code_block(id: &'static str, text: String, cx: &App) -> Stateful<Div> {
    let t = cx.theme();
    div().id(id).w_full().p(px(ui::sp::M)).rounded(t.radius).border_1().border_color(ui::hairline(cx)).bg(t.muted)
        .text_size(px(ts::CAPTION)).font_family(t.mono_font_family.clone()).child(text)
}

fn note_el(n: &Option<(bool, String)>, cx: &App) -> Option<Div> {
    n.as_ref().map(|(ok, s)| div().text_size(px(ts::CAPTION))
        .text_color(if *ok { cx.theme().muted_foreground } else { fmt::money_out() }).child(s.clone()))
}

impl Render for Assistants {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let lede = "Assistants such as Claude or Codex can read your finances through MCP: read-only (they cannot change anything or \
            move money), and only from devices on your tailnet. Each assistant gets its own key, which you can revoke any time.";
        let page = |left: Div, right: Option<Div>| {
            let col = || v_flex().flex_1().min_w(crate::layout::col(360.)).gap_2();
            h_flex().w_full().gap_4().items_start().flex_wrap().child(col().child(left)).children(right.map(|r| col().child(r)))
        };

        // no server (the demo): nothing to switch on
        if !self.stub && finance_core::demo::active() {
            return page(
                v_flex().gap_2().child(ui::section("AI assistants", "MCP", cx)).child(ui::card(cx)
                    .child(ui::caption_wrap(lede, cx))
                    .child(ui::caption_wrap("The demo has no server. Sign in to your server to switch MCP on, or use local mode.", cx))),
                self.local_card(cx),
            );
        }

        if !self.asked && self.status.is_none() {
            self.asked = true;
            self.refresh(cx);
        }

        let intro = ui::card(cx).gap_2().child(ui::caption_wrap(lede, cx));
        let mut left = v_flex().gap_2().child(ui::section("AI assistants", "read-only, on your tailnet", cx));

        let status = match &self.status {
            None => return page(left.child(intro).child(ui::card(cx).child(ui::caption_wrap("Loading…", cx))), None),
            Some(Err(e)) => {
                return page(left.child(intro).child(ui::card(cx).child(div().text_size(px(ts::CAPTION)).text_color(fmt::money_out())
                    .child(format!("Couldn't read the MCP settings: {e}")))), self.local_card(cx));
            }
            Some(Ok(s)) => s.clone(),
        };

        let switch = Switch::new("mcp-switch").with_size(ui::switch_size())
            .checked(status.enabled)
            .label("Let assistants read my data")
            .on_click(cx.listener(|this, on: &bool, _, cx| this.set_enabled(*on, cx)));
        left = left.child(intro.child(switch).children(note_el(&self.note, cx)));
        if !status.enabled {
            return page(left.child(ui::card(cx).child(ui::caption_wrap("Off: the server answers /mcp with nothing, and every key is refused. Your keys are kept.", cx))), None);
        }

        // ---- keys ----
        let now = now_ms();
        let rows = status.keys.iter().map(|k| {
            let id = k.id;
            let confirming = self.confirm_revoke == Some(id);
            v_flex().gap_1().py_1()
                .child(h_flex().gap_2().items_center()
                    .child(v_flex().flex_1().min_w_0().gap(px(2.))
                        .child(div().text_sm().font_semibold().overflow_hidden().text_ellipsis().whitespace_nowrap().child(k.name.clone()))
                        .child(ui::caption(format!("made {} · used {}", mcp::relative(now, Some(k.created)), mcp::relative(now, k.last_used)), cx)))
                    .when(!confirming, |d| d.child(Button::new(("mcp-rm", id as usize)).small().ghost().label("Revoke")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_revoke = Some(id);
                            cx.notify();
                        })))))
                .when(confirming, |d| d.child(h_flex().gap_2().items_center().flex_wrap()
                    .child(div().text_size(px(ts::CAPTION)).text_color(t.warning).child("Revoke it? The assistant using it loses access at once."))
                    .child(Button::new(("mcp-rm-go", id as usize)).small().danger().label("Revoke?")
                        .on_click(cx.listener(move |this, _, _, cx| this.revoke(id, cx))))
                    .child(Button::new(("mcp-rm-no", id as usize)).small().ghost().label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_revoke = None;
                            cx.notify();
                        })))))
        }).collect::<Vec<_>>();
        let n = rows.len();
        let mut keys = ui::card(cx).gap_1();
        if n == 0 {
            keys = keys.child(ui::caption_wrap("No keys yet. Make one for each assistant.", cx));
        }
        for (i, r) in rows.into_iter().enumerate() {
            keys = keys.child(r);
            if i + 1 < n {
                keys = keys.child(div().h(px(1.)).bg(ui::hairline(cx)));
            }
        }
        let keys_part = v_flex().gap_2().child(ui::section("Keys", "one per assistant", cx)).child(keys);

        // ---- make a key ----
        let choices = h_flex().gap_2().flex_wrap().children(AssistantClient::ALL.into_iter().enumerate().map(|(i, c)| {
            Button::new(("mcp-client", i)).small().outline().label(c.label())
                .when(self.client == c, |b| b.icon(IconName::Check))
                .on_click(cx.listener(move |this, _, window, cx| this.pick_client(c, window, cx)))
        }));
        let make = ui::card(cx).gap_2()
            .child(ui::field_row("Assistant", choices, cx))
            .child(ui::field_row("Key name", Input::new(&self.name).small(), cx))
            .child(h_flex().gap_2().items_center()
                .child(Button::new("mcp-create").small().outline().icon(IconName::Plus).label("Create key")
                    .loading(self.creating).disabled(self.creating)
                    .on_click(cx.listener(|this, _, _, cx| this.create(cx))))
                .child(ui::caption("the key is shown once", cx)));
        let right = v_flex().gap_2().child(ui::section("New key", "the assistant sets the default name", cx)).child(make);
        let mut panel = None;

        // ---- the one-time panel ----
        if let Some(f) = &self.fresh {
            let c = f.client;
            let snip = mcp::snippet(c, &f.url, &f.token);
            let alt = mcp::alt_snippet(c, &f.url, &f.token);
            let prompt = mcp::agent_prompt(c, &f.url, &f.token);
            let copy_btn = |id: &'static str, label: &'static str, text: String, cx: &mut Context<Self>| {
                let done = f.copied == Some(id);
                Button::new(id).small().outline()
                    .icon(if done { IconName::Check } else { IconName::Copy })
                    .label(if done { "Copied" } else { label })
                    .on_click(cx.listener(move |this, _, _, cx| this.copy(id, text.clone(), cx)))
            };
            let test = match &self.test {
                Test::Idle => None,
                Test::Running => Some(ui::caption("testing…", cx)),
                Test::Done(Ok(s)) => Some(div().text_size(px(ts::CAPTION)).text_color(fmt::money_in()).child(s.clone())),
                Test::Done(Err(e)) => Some(div().text_size(px(ts::CAPTION)).text_color(fmt::money_out()).child(e.clone())),
            };
            panel = Some(v_flex().gap_2().child(ui::section("Your new key", &f.name, cx)).child(ui::card(cx).gap_2()
                .child(div().text_size(px(ts::CAPTION)).text_color(t.warning).child("You won't see this key again. Copy it now; Done wipes it from this window."))
                .child(code_block("mcp-key", f.token.clone(), cx))
                .child(h_flex().gap_2().flex_wrap()
                    .child(copy_btn("copy-key", "Copy key", f.token.clone(), cx))
                    .when(!web(), |d| d.child(Button::new("mcp-test").small().outline().icon(IconName::Globe).label("Test")
                        .loading(matches!(self.test, Test::Running))
                        .tooltip("initialize and list the tools, with this key")
                        .on_click(cx.listener(|this, _, _, cx| this.run_test(cx)))))
                    .children(test))
                .child(div().h(px(1.)).bg(ui::hairline(cx)))
                .child(h_flex().gap_2().flex_wrap().children(AssistantClient::ALL.into_iter().enumerate().map(|(i, k)| {
                    Button::new(("mcp-setup-client", i)).small().outline().label(k.label())
                        .when(c == k, |b| b.icon(IconName::Check))
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_setup_client(k, cx)))
                })))
                .child(ui::caption_wrap(c.where_to(), cx))
                .child(code_block("mcp-snippet", snip.clone(), cx))
                .when_some(alt, |d, a| d
                    .child(ui::caption_wrap("Or, in ~/.codex/config.toml:", cx))
                    .child(code_block("mcp-snippet-alt", a.clone(), cx))
                    .child(h_flex().child(copy_btn("copy-alt", "Copy config block", a, cx))))
                .child(h_flex().gap_2().flex_wrap()
                    .child(copy_btn("copy-snippet", "Copy setup", snip, cx))
                    .child(copy_btn("copy-prompt", "Copy a prompt for the agent", prompt, cx)))
                .child(ui::caption_wrap("The prompt tells an agent how to add this server itself: paste it into the assistant's chat. It contains the key.", cx))
                .child(h_flex().child(Button::new("mcp-done").small().label("Done")
                    .on_click(cx.listener(|this, _, _, cx| this.forget(cx)))))));
        }
        // the new key first: it is what the user just asked for
        page(left.children(panel).child(keys_part), Some(right))
    }
}
