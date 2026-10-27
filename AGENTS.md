# finance — notes for agents

Personal-finance app, all Rust: one Cargo workspace in `desktop/`
(`engine` store/ingest/valuation, `server` = `anser-server`, `core` client
logic, `app` GPUI pages, `web` the same pages as wasm, `shots` optional
screenshots). Start here:

- **README.md**: what the app does, architecture, data sources, commands, tests.
- **desktop/README.md** (the app, page by page) and
  **desktop/server/README.md** (API, CLI, accounts, archives, tests).
- **DESIGN.md**: the UI's principles (tokens, sections, both layouts). Follow it for any page work.
- **desktop/docs/gpui-kit-0.7-notes.md**: read before touching GPUI code.
- **DEPLOY.md**: production is your server (its ssh host is in the
  gitignored `scripts/deploy.local`). Ship with `scripts/deploy.sh`, and only
  committed `main`. Never hand-edit code on the server.
- **knowledge/**: long-term memory about the user's finances plus feature
  ideas (`knowledge/ideas/`). Read `knowledge/README.md` before analysis or
  feature work, and update the relevant file when you learn something durable.
  Everything in it except the README is gitignored. Keep it that way.

## Working

- Build and test from `desktop/`: `cargo test --workspace`; with
  `FINANCE_PARITY_DIR=$PWD/../data/parity` the optional private golden tests run
  too and must stay green (they pin one install's output byte for byte). The
  web build: `RUSTC_BOOTSTRAP=wasm_thread cargo check -p finance-web --target wasm32-unknown-unknown`.
- Build the server with `-p finance-server`; GPUI makes the app crate slow.
- Screenshots of the app: the demo household only (`finance-shots`, or
  `desktop/scripts/shot.sh`).

## Rules

- Personal data (`transactions/`, `data/`, `.env`,
  `knowledge/*`) never goes into git, nor anything derived from it (goldens,
  fixtures, screenshots, docs with real amounts or names).
- The laptop `data/finance.db` is a dev copy and may lag prod. For analysis,
  pull a snapshot: run `~/.local/bin/anser-server backup` on the server, then scp it. Open it read-only.
- API keys are read-only. Nothing in this app may place trades or move money.
- Pages must never 500: fall back to cached data with a stale badge.
