# DESIGN.md — one set of pages, native and in the browser

How the app's UI is built and what a page must respect. The pages are Rust
(GPUI + gpui-kit) in `desktop/app/src/`; the same code runs as the native
desktop app and, compiled to wasm, as the web app on a phone. There is no
HTML, CSS or JavaScript UI to maintain (the web build's only JS is
wasm-bindgen's glue and the service worker).

Read [desktop/docs/gpui-kit-0.7-notes.md](desktop/docs/gpui-kit-0.7-notes.md)
before writing GPUI code, and [desktop/README.md](desktop/README.md) for what
each page does.

---

## 1. Principles

1. **Instant, local first.** A page renders from the local replica (the
   SQLite snapshot and the export bundle) before anything else; the window
   never flashes empty. Syncing happens in the background and a new load
   simply re-renders.
2. **Never fail, show staleness.** A failed fetch falls back to cached
   data with a visible stale badge or sync state; a page always renders.
   The server answers from caches too: no page depends on the network.
3. **The server computes, the client presents.** Anything that needs the
   valuation model comes from the bundle (`finance_engine::export`); pages
   never re-derive net worth or investment values. Cheap client-side
   computations (ledger scans, budgets, recurring, travel) live in
   `finance-core`, UI-free and tested.
4. **One design system, tokenized.** Colors come from the theme
   (`app/src/theme.rs`: a palette with dark and light variants, an accent
   source over it); sizes from the scales in `app/src/ui.rs` (`sp::*` for
   spacing, `ts::*` for type). Components reference tokens, never literal
   colors or ad-hoc numbers.
5. **Color carries meaning, nothing else.** Quiet surfaces, one accent.
   Money in is green and money out red whatever the accent (`fmt.rs`), and
   always with a sign, so color is never the only signal.
6. **Both layouts, always.** Under 600 logical px (`layout.rs`) the app is
   compact: a bottom tab bar instead of the sidebar, one column of tiles,
   lists instead of wide tables, touch targets of at least `layout::TOUCH`.
   Every page must work in both.
7. **Explain the numbers.** A metric that isn't obvious gets an ⓘ
   explainer (`Tile::explained`, `ui::stat_info`, `ui::with_info`) that says
   what it is and how it is computed.
8. **Private by default.** No third-party requests from the UI; fonts are
   bundled (`web/fonts/`), logos and map data ship with the app. Privacy
   mode (Ctrl+H) masks every amount, so a screen can be shared.

---

## 2. Visual language

- **Surfaces**: `ui::card` (a hairline border, the theme's raised
  surface), `ui::card_interactive` for anything clickable, `ui::card_slice`
  to cut a long card into rows for the virtualized list.
- **Labels**: `ui::eyebrow` (small uppercase above a figure),
  `ui::caption` (one muted line, ellipsized) and `ui::caption_wrap` (a note
  that wraps).
- **Figures**: tabular, lining numerals (`.figures()`, `ui::tnum()`) on
  every number that sits in a column or changes, so digits never jitter.
  One hero figure per page at most (`ts::HERO`).
- **Themes**: warm (the default), nord, catppuccin, gruvbox, tokyo-night,
  rose-pine, glass (translucent; the compositor blurs what is behind) and
  terminal (near-black, amber used sparingly, all text monospace; its
  `theme::Look` squares the cards, draws section headings with box rules,
  puts chips and deltas in brackets and the page title behind a prompt, so
  use `ui::section`, `ui::chart_card`, `ui::chip` and `ui::delta` rather
  than hand-made ones), each dark and light. The accent can follow matugen (re-themed live when
  the wallpaper changes) or a custom hex. Test a page in at least one dark
  and one light palette.
- **Zoom**: the whole UI scales (`zoom = 1.0`, Ctrl +/-/0). Every file that
  uses `px(` imports `crate::zoom::px`, or it won't zoom.
- **Motion: none when the data is ready** (a core principle). Data that is
  already there is drawn in its final state at once: charts don't draw in,
  bars don't grow, numbers don't count up. Motion is only for waiting and
  for things the user is doing: the loading watermark while a first load is
  slow, a spinner while a file parses, a live theme preview. Nothing bounces.

---

## 3. Pages

- **A page is a list of sections.** It returns `ui::Sections` (its blocks,
  top to bottom); the app draws them as a virtualized list, so a frame lays
  out only what is on screen. GPUI renders every frame of a scroll, so a
  page's render runs up to 60 times a second.
- **Memoize computation.** Sums, sorts and ledger scans go through
  `memo::get(tag, &inputs, || ...)`: kept while the inputs are the same,
  cleared on every new load. `FINANCE_DESKTOP_RENDER_TIMING=1` (web:
  `?timing=1`) prints each page's build time.
- **State is cheap view state.** Tabs, the month, a search, an open deep
  dive: kept on the page and recorded in the jump list (Ctrl+O / Ctrl+I),
  not in globals.
- **Keyboard first on the desktop.** The main pages have keys (Ctrl+1…),
  search and the palette reach everything; key hints disappear on touch-only browsers and
  the compact layout (`ui::show_shortcuts()`).
- **Platform differences** stay at the module edge: a page that can't run
  in the browser has a `*_web.rs` stand-in selected by
  `#[cfg_attr(target_family = "wasm", path = ...)]` (`pages/mod.rs`).
- **Capabilities hide what has no data.** A page or source the server has
  nothing for is hidden (`pages::set_capabilities`), never shown empty by
  default; an empty state explains how to get data in.
- **Dev aids, not clicks.** Every page, tab and dialog can be opened from
  an environment variable (`FINANCE_DESKTOP_PAGE`, `..._TAB`, ...; the web
  app takes the same as query parameters), so screenshots and checks never
  type into the window. Add one for anything new.

---

## 4. Charts

- **One value axis, always.** If two series don't share a scale, use two
  charts.
- **Fixed, meaningful colors**: an entity or category keeps its color on
  every chart; positive and negative use the money colors with a sign.
- **Every value reachable**: hover readouts on marks, and the numbers in a
  table or list nearby, so nothing needs a pointer.
- **Graceful empties**: no data gives an explanation, not a broken axis.
  Labels that don't fit are dropped or shortened, never allowed to panic
  the layout.

---

## 5. Checklist for a page change

- [ ] Renders from the replica alone (offline, stale data, an older server).
- [ ] Works compact (390 px wide) and on the desktop, dark and light.
- [ ] Money colored by sign and meaning only; figures tabular.
- [ ] Heavy work memoized; render timing checked on a big page.
- [ ] A dev aid opens the new state; a screenshot of the demo shows it
      (`desktop/scripts/shot.sh`, or `finance-shots` for the web build).
- [ ] No personal data in code, tests, docs or screenshots.
