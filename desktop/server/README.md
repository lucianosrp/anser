# finance-server (`anser-server`)

The finance server: the HTTP API, the export bundle, background sync, the
CLI and MCP (stdio, and over HTTP at `/mcp`), over one SQLite database per user. The engine (store,
parsers, ingest, categorize, valuation views, fetchers) lives in
`../engine`. This crate adds the HTTP server, background sync, push, the web
app's hosting and the `/mcp` endpoint with its keys (the MCP tools themselves
are the engine's).

The browser UI is the web app: the desktop app's own pages built for wasm,
served at the server root (see "The web app" below). The server renders no
HTML pages of its own.

```bash
cd desktop
cargo build -p finance-server --release      # never the app crate: GPUI is slow to build
target/release/anser-server serve              # 127.0.0.1:8787
```

## Commands

The same commands run from the desktop app as `anser server <cmd>` (the CLI
is the library's `finance_server::cli`; usage lines name `anser server` there).

| Command | What |
|---|---|
| `serve [--bind HOST:PORT] [--push-to URL] [--push-every SECS]` | the API and the web app. Default bind `127.0.0.1:8787` (also the desktop app's default server) |
| `mcp` | the read-only MCP server on stdio (see MCP): no key, no switch |
| `mcp-key create NAME \| list \| revoke ID`, `mcp-enable`, `mcp-disable` | a user's MCP keys (the token is printed once) and the switch for `/mcp` over HTTP, off by default (see MCP) |
| `ingest`, `categorize [--reset --yes-replace-rules]`, `txns`, `override`, `stats`, `report [--from --to]`, `backup [--keep N]` | load new `transactions/` files; (re)apply the category rules (`--reset` wipes all rules for the generic defaults and needs `--yes-replace-rules`); list and override transactions; row counts; a monthly report; an online db copy into `data/backups/` (newest N kept) |
| `fetch-crypto [--exchange binance\|okx\|ibkr\|all] [--full]`, `fetch-fx`, `refresh-prices [--range 5y]` | the exchange and broker fetchers, monthly FX, daily closes |
| `sync` | every fetcher in the foreground, under the server's sync lock |
| `push [--remote URL]` | send the remote the local files and overrides it lacks |
| `install-webapp --dir DIR --server URL` | send a built web app bundle (`build-web.sh` output) to a server's `POST /api/webapp`; a `.ts.net` name resolves through MagicDNS like `push` |
| `pair [--user NAME\|ID]` | a one-time 8-character pairing code, valid 10 minutes, for one device (another user's with `--user`); also written to `$FINANCE_HOME/secrets/pair-code` (0600). On a fresh install nobody has claimed yet, it first makes the shell's user the owner (user 1, admin) |
| `user list`, `user revoke ID` | users and their paired devices, and how new accounts are made here; revoke a device |
| `user add NAME [--email E] [--admin]`, `user rename NAME\|ID NEW`, `user remove NAME\|ID [--purge --yes-delete-their-data]` | a household member with their own tree (`users/<id>/`, no invite needed on the server's shell); rename; remove (devices and account go, the tree and secrets stay unless purged; user 1 can't be removed) |
| `--user NAME\|ID` on `mcp`, `mcp-key`, `mcp-enable`, `mcp-disable`, `sync`, `backup`, `export`, `import`, `ingest`, `categorize`, `txns`, `override`, `stats`, `report`, `fetch-*`, `refresh-prices` | run on that user's tree; without it, the owner's (as always) |
| `auth [off\|writes\|all]` | show or store the auth mode (`FINANCE_AUTH` still wins) |
| `secrets status`, `secrets import-env` | where each source key comes from; move the keys from `.env` into the encrypted store (keeps `.env.bak`, comments the lines out) |
| `export [--out FILE] (--passphrase-file F \| --recipient age1…) [--from D] [--to D] [--sources a,b] [--with-secrets]` | the account as one encrypted `.finance` archive (see Archives). With no key given, the configured backup recipient. `--passphrase-file -` reads stdin |
| `import FILE --mode fresh\|merge\|replace [--user NAME\|ID] [--dry-run] [--prefer-archive] (--passphrase-file F \| --identity FILE)` | an archive into this tree. Merge and replace take the sync lock |
| `verify FILE (--passphrase-file F \| --identity FILE)` | open and check an archive: the age MAC, every entry's sha256, the database's integrity and row counts |
| `backup [--keep N] [--user NAME\|ID]` (with a recipient) | also writes `data/backups/archives/finance-*.finance`, sealed to the backup recipient, keeping the newest N (`backup_archive_keep`, default 14). Without `--user`: the owner's tree as always, then every other user's into their own `users/<id>/data/backups/`, sealed to their own recipient |

## Environment

| Variable | What |
|---|---|
| `FINANCE_HOME` | data tree root (`data/`, `transactions/`, `.env`). Default: the repo root when built from a checkout, else `~/.local/share/finance` |
| `FINANCE_DB`, `FINANCE_DATA_DIR`, `FINANCE_TRANSACTIONS_DIR` | override one part of the tree |
| `FINANCE_BIND` | bind address when `--bind` is absent |
| `FINANCE_REMOTE` | default `push` target (`.env` or environment) |
| `FINANCE_TODAY=YYYY-MM-DD` | pins "today" for every view (golden tests) |
| `FINANCE_OFFLINE=1` | no network: fetchers refuse, views serve their caches, no auto-sync |
| `FINANCE_AUTH` | `off`, `writes` or `all` (see Accounts); overrides the mode stored in server.db |
| `FINANCE_ALLOW_SIGNUP` | `1`/`true`/`yes`/`on`: anyone who reaches the server may make an account without an invite. Default off (see Accounts) |
| `FINANCE_ALLOWED_HOSTS` | extra Host names to answer to, comma-separated (besides localhost, IP literals, `*.ts.net`, this machine's hostname) |
| `FINANCE_MASTER_KEY_FILE` | the secrets master key (default `$FINANCE_HOME/secrets/master.key`) |
| `FINANCE_TOKEN` | the device token `push` sends to the remote (else the desktop's `tokens.json`) |

API keys come from the encrypted secrets store, then `.env`, then the
environment (see Secrets). All of them are read-only, and every exchange
or broker call is a GET.

## Accounts

`data/server.db`, a separate SQLite file next to finance.db: never in
`/api/db.sqlite`, never served, not in `anser-server backup`'s output.

- `users` (id, name, role, home, created, email), `devices` (id, user_id,
  name, token_hash, scopes, created, last_seen), `pair_codes` (code hash,
  expires, used), `reconnects` (user_id, device_id, device_name, code
  hash, resume key hash, created, fails: a device signed out with a code to
  jump back in), `invites` (code hash, created_by, expires, used, used_by).
- **First start** (`auth::bootstrap`, run by `serve`, `pair`, `user`): an
  existing tree becomes user 1, admin, home = `FINANCE_HOME`, in place (no
  file moves). A fresh install (no finance.db at the first start) has no
  users until it is claimed: the first account made in the app, or
  `anser-server pair` (the shell's user). Before finance.db is touched, a backup lands in
  `data/backups/pre-auth/` (never pruned). finance.db gains the `profile` and
  `user_settings` tables (nothing else changes; `server/tests/owner_migration.rs`
  checks it on a copy of the owner's db), and the owner's profile is seeded: HKD, HK,
  the name from the self-transfer rules.
- **Pairing**: `anser-server pair` (or `POST /api/admin/pair-code`) makes a
  one-time 10-minute code; `POST /api/pair {code, device_name}` returns a
  random 256-bit bearer token (`fin_` + 64 hex; only its SHA-256 is stored).
  A browser posts the code to `POST /api/login` (with `X-Finance-Request`)
  and gets a session cookie (HttpOnly, Secure, SameSite=Strict); `/login`
  is a tiny page for that (a fallback: the web app signs in on its own
  screen). Ten wrong codes (pairing, jump-back-in, or invite) in 10
  minutes lock all of them for the rest of the window. A refusal carries
  `reason`: `wrong`, `used`, `expired`, `locked`, `throttled`.
- **Signing out, and jumping back in** (`auth::sign_out`): `POST
  /api/logout {resume_key?}` revokes the caller's device (and clears the
  cookie). With a resume key (32 to 128 letters, digits, `-`, `_`: a
  random string the client keeps; the desktop next to its tokens as
  `URL#resume`, the browser in localStorage) the reply carries
  `reconnect_code`, an 8-character code the client shows ("Here's the code
  to jump back in"). `/api/pair` and `/api/login` take it like a pairing
  code, but only with that same `resume_key` in the body (clients send
  theirs with every sign-in), and once: it works only on the device that
  signed out, and has no time limit. One per signed-out device and user
  (signing out again replaces it; signing back in on that device by any
  code spends it). It ends when an admin signs that device out (`DELETE
  /api/admin/devices/{id}`, the id the device had; `GET /api/admin/devices`
  lists these under `signed_out`), when the user is removed, with Delete
  all, or after 5 wrong codes sent with its resume key (`locked`; those
  count toward the ten above too). Only SHA-256 hashes of the code and the
  key are stored. Without a resume key (*Sign out for good*, an older
  client) nothing is left behind.
- **New accounts** (`accounts.rs`): `POST /api/signup` {name, email?,
  invite?, device_name?} answers a device token (the desktop),
  `POST /api/signup/session` a session cookie (a browser; needs
  `X-Finance-Request`). With no users at all, the first account needs
  nothing and becomes user 1, admin, owner of this tree. After that an
  account needs an invite (`POST /api/admin/invite`, admins: one account,
  24 hours) unless `FINANCE_ALLOW_SIGNUP` is on (refused with `reason:
  closed` otherwise). `GET /api/whoami` says which (`signup`: `first`,
  `open`, `invite`).
- **A household: one data tree per user** (see the next section). Every
  account gets its own tree when it is made; one made before per-user
  trees gets it at the next start. A device without a tree (a safety net)
  gets only the public routes.
- **Modes** (`FINANCE_AUTH`, else the stored default): `all` (the default,
  for new installs and for one that had a finance.db before accounts
  existed: everything but `/healthz`, `/api/version`, the web app's files,
  `/login`, `/api/whoami` and pairing needs a device), `writes` (every
  non-GET needs a device, reads stay open), `off`. Under `writes` the
  Doctor warns that reads are open to every tailnet device. Switch with
  `anser-server auth writes|all`, `POST /api/admin/auth-mode`, or Settings →
  Admin → Lock reads. A server.db that can't be read counts as `all`.
- **The gate** (`gate.rs`): unknown Host headers get 421 in every mode (DNS rebinding);
  a non-GET with a foreign Origin gets 403; a cookie session's non-GET needs
  `X-Finance-Request`. Secret routes (`/api/sources/{key}/secret|test`) and
  admin routes (`/api/admin/*`, `/api/secrets/import-env`, `/api/webapp`)
  always need a device, whatever the mode, and secret writes are refused
  unless the peer is loopback (`tailscale serve` proxies to loopback).
- `serve` sets umask 077: everything it creates is private to its user.

## Users and their trees (a household server)

One server, several people, one SQLite db each (`engine/src/tenant.rs`,
`server/src/homes.rs`):

```text
$FINANCE_HOME/                  user 1, the owner: the tree as it always was, in place
  data/finance.db               (every FINANCE_* override still applies to it)
  data/server.db                users, devices, codes: server-wide
  data/webapp/                  the web app: server-wide
  secrets/<uid>.json.enc        every user's keys, one master.key
  users/<uid>/data/finance.db   everyone else: the same layout under users/<uid>/
  users/<uid>/transactions/     (fetch_status.json, sync.lock, backups/ in data/)
  users/<uid>/formats/
  users/<uid>/avatar/
```

- **Whose data a request touches.** Every request runs as its device's
  user: server.rs makes that user's `Tenant` current for the request
  (`Tenant::enter`), and every tree path (config's home, data dir, db,
  transactions, the formats dir, the secrets lookup, the avatar) resolves
  through it, so db::open, the views, ingest, categorize, the fetchers'
  store, export and the archive all land on that user's tree. Background
  work (the live valuation, the warmers, a sync, a Gravatar refresh, the
  sweep's exchange calls) carries its tenant to its thread
  (`tenant::spawn`). The server runs in strict mode: a tree path resolved
  on a thread with no tenant panics (500, nothing read) instead of falling
  back to the owner's tree. A CLI command runs as the owner unless
  `--user` names someone else.
- **Per user**: the db (profile, user_settings, categories, rules,
  data_rev, the investment setup), the views cache (keyed by db path and
  fingerprint), the live valuation, the sync state, `sync.lock` and
  `fetch_status.json`, the warmers, the formats, the avatar, the secrets
  (the owner's `.env` and the process environment count for the owner
  only), the backup recipient (`backup_recipient:<uid>` in server.db's
  meta; the owner keeps `backup_recipient`), backups and archives. Market
  data (FX, prices) is fetched into each db (duplicated; small).
- **A new user's tree** (an invite accepted, self-signup, `anser-server user
  add`): the schema, the categories table, empty investment tables (none
  of the owner's accounts, instruments or rules), a profile with their name
  and no settings; the onboarding wizard does the rest. Ids are never
  reused (server.db meta `last_uid`), so a removed user's kept tree never
  passes to someone new. A user row whose home isn't exactly
  `users/<its id>` is refused (no tree).
- **A household server is always `all`.** With more than one tree a
  request without a device is refused data whatever `FINANCE_AUTH` says
  (`auth::effective_mode`; whoami's `household`): it would otherwise be
  read as the owner's.
- **Admins manage accounts, not data.** `GET /api/admin/users` (names,
  roles, device counts; no paths), `PATCH /api/admin/users/{id}` {name},
  `DELETE /api/admin/users/{id}` (devices and account go, the tree stays
  on disk), `GET /api/admin/devices`, `DELETE /api/admin/devices/{id}`,
  invites, pairing codes, the auth mode, the web app. No route reads
  another user's data, admin or not; there is no impersonation. On disk,
  whoever runs the server (the admin, on the server) can read every tree and,
  with master.key, every user's secrets: the separation is between the
  app's users, not against the machine's owner.
- **Any user's own**: the archive (`/api/account/export|import`), the
  backup key and archives (`/api/admin/backup-recipient`,
  `/api/admin/archives*`: Secret class, own tree), the investment setup
  (`/api/admin/investments/*`: a write), a pairing code for another of
  their devices (`POST /api/me/pair-code`).
- **Sync**: `POST /sync` syncs the caller's tree; a scheduler thread walks
  every user with an API source configured every 15 minutes and syncs a
  ledger older than a day; at most 2 syncs run at once across users (a
  semaphore), one per user (a flag and that user's `sync.lock`). The log
  says `[sync]` for the owner, `[sync user N]` for others.
- **MCP**: `anser-server mcp --user NAME|ID` serves that user's tree (default
  the owner); over HTTP a key belongs to one user and `/mcp` runs as that
  user (see MCP).
- **Proof** (`server/tests/isolation.rs`): every route called as another
  household member, writes and dry runs included, leaks none of the
  owner's markers and leaves the owner's tree unchanged row for row; a
  second admin reads nothing of anyone else's; anonymous requests on a
  household server get no data; MCP reads one tree (`mcp --user`, and a
  user's key over HTTP). It also scans
  the server's source for route literals: a new route that isn't on its
  list fails the test until it is added there (and so called as another
  user).

## MCP

Read-only: no tool writes to the database, places a trade or moves money.
The server speaks MCP protocol versions 2025-11-25, 2025-06-18, 2025-03-26
and 2024-11-05 (the client's when supported, else the newest), statelessly.
Results are one text block of compact JSON plus `structuredContent`; a bad
argument or an unknown name is an `isError` result with a message that says
what is valid. Money is in the base currency (stated once, at the top),
rounded to 2 decimals, percentages to 1, empty fields left out. Months are
`YYYY-MM`, dates `YYYY-MM-DD`; a month range defaults to the last 12
complete months.

| Tool | What |
|---|---|
| `summary` | start here: net worth and its classes (cash, investments, crypto, owed to you, liabilities, property), this month so far and the trailing 12 months (income, spending, savings, savings rate), how fresh the data is (stale prices and sources), and the exact account, category and investment-account names the other tools take |
| `trend` | one monthly series (`net_worth`, `income`, `spending`, `savings`, `savings_rate`, `category:<name>`, `account:<name>` balance) with min, max, average and the first-to-last change |
| `breakdown` | totals by `category`, `merchant`, `account` or `currency`, for spending or income: share, count and the change against the previous period of equal length; top N and the rest as Other |
| `transactions` | bank and card rows, filtered (text, account, category, dates, absolute amount, direction), newest or largest first, paged by `next_cursor`; the matching total |
| `recurring` | subscriptions, bills and other recurring charges (core `recurring::detect`): cadence, typical amount, monthly cost, last and next date, price changes, and the monthly total |
| `holdings` | positions across every broker, fund and exchange: quantity, price, value, cost, unrealised gain, weight; filter by account or asset class, or group by account, asset class or currency |
| `performance` | value per month, contributions and gain, time-weighted and money-weighted return, the benchmark, income received, fees and cash drag; for the whole portfolio or one account; approximations are labelled in `notes` |
| `activity` | the investment ledger, newest first: buys, sells, deposits, withdrawals, dividends, interest, fees, taxes, crypto rewards; with a symbol, the DCA summary (core `dca`) |

No tool invents financial math. Net worth, the monthly balance sheet and the
investment returns come from the views, through the same fingerprint cache
the pages use (a call on a warm cache is a few milliseconds; the test prints
the p50 per tool). The one definition added is income and spending: every
non-transfer bank row, the owner's share, at the month's rate, which is the
spending page's trend, so `summary`, `trend` and `breakdown` always agree.
Left out: a `breakdown` by trip (the trip detection needs the home country,
not available cheaply), and holdings by region or sector.

**Over stdio**: `anser-server mcp [--user NAME|ID]`, one JSON-RPC message per
line. It is shell access already: no key, no switch. The loop is the engine's
(`mcp::run_stdio`); the desktop app's `anser mcp [--account NAME]`
runs the same one over its local copy (local mode, no server involved; see
desktop/README.md, Assistants).

**Over HTTP**: `POST /mcp` takes one JSON-RPC message or a batch and answers
`application/json` (the Streamable HTTP spec's JSON response mode: no SSE,
so `GET /mcp` is 405); a POST of only notifications is 202 with no body. No
session id; `MCP-Protocol-Version` is accepted and ignored.

- Auth (gate.rs, class `Mcp`): a bearer token, whatever the auth mode, and
  nothing else: no cookie session (403), no anonymous call (401). The Host
  must be on the allow-list even where the mode doesn't ask for it, and an
  `Origin`, when sent, must be on it too (DNS rebinding). The request runs
  as the token's user (their tenant).
- The switch: per user, `{"enabled": bool}` in `user_settings` key `mcp`
  (not one of `SETTING_KEYS`, so it stays out of the export bundle),
  default off. Off, `/mcp` answers 404 exactly like an unknown route.
  `PUT /api/mcp` or `anser-server mcp-enable|mcp-disable`.
- MCP keys: a device row (`server.db`) with scope `mcp`, `fin_` plus 64 hex,
  only its SHA-256 kept. gate.rs refuses it (403) on every route but
  `/mcp`, as a bearer and as a cookie. At most 20 per user.
- Key API, for Settings (a normal token or cookie; the writes follow the
  usual rules: a cookie needs `X-Finance-Request`, and making or revoking a
  key is a Secret-class write, loopback or TLS only): `GET /api/mcp` ->
  `{enabled, path: "/mcp", keys: [{id, name, created, last_used}]}` (never
  a secret); `PUT /api/mcp` `{"enabled": bool}`; `POST /api/mcp/keys`
  `{"name"}` -> `{id, name, created, token}` (the token this once);
  `DELETE /api/mcp/keys/{id}`. Every one acts on the caller's own user: a
  key of another user, or a paired device's id, is a 404.
- Not logged: the log line of a request is its method, path, status and
  time; no header, body or token.
- Exposure is the server's: tailnet only (DEPLOY.md, "MCP for an
  assistant"). There is no OAuth: a client sends the key as
  `Authorization: Bearer`.

## Secrets

`$FINANCE_HOME/secrets/<uid>.json.enc` (XChaCha20-Poly1305, a fresh nonce
per write, the user id as associated data), master key
`$FINANCE_HOME/secrets/master.key` (32 random bytes, 0600, created on the
first write; the dir is 0700). A key file, not a passphrase, so the server
restarts unattended. Refused inside the data dir; never in finance.db.

- Provider order per key: the store, then `.env`, then the environment
  (the owner's tree; any other user's keys come from their store only);
  `/api/capabilities` and `/api/sources` report `configured_via`. Writes are
  live on the next read (cached on the file's mtime); `.env` is re-read when
  it changes, too.
- `GET /api/sources/catalog`: each API source's fields (labels, help, which
  are secret), how to make a read-only key, what the test refuses; every
  file source with export instructions.
- `GET /api/sources`: per key `set`, `via`, and (to a paired device only) a
  hint of the last 4 characters.
- `POST /api/sources/{key}/test` (candidate fields, else the stored ones)
  and `PUT /api/sources/{key}/secret` (tests first; 422 on failure):
  Binance `GET /api/v3/account` + `/sapi/v1/account/apiRestrictions`,
  refusing trading, withdrawal, internal/universal transfer, margin, futures
  or options; OKX `GET /api/v5/account/config` (`perm` must be exactly
  `read_only`) + the balance; IBKR Flex SendRequest + GetStatement, with the
  sections present. `DELETE /api/sources/{key}/secret`.
- No reply, log line or error carries a value; test errors are scrubbed of
  the candidate values (`server/tests/auth_routes.rs` checks it).

## Archives (export, import, nightly backups)

One `.finance` file holds a user's tree, encrypted (`finance_engine::archive`).
It's in the age format, sealed to a passphrase (scrypt) or to X25519
recipients (`age1…`). Inside is a zstd tar:

- `manifest.json`: format_version, app version, bundle_version, the
  schema (table_info per table), row counts, the sha256 and size of every
  entry, the scope, includes_secrets, created_at, the base currency.
- `db/finance.sqlite`: a consistent snapshot (the backup API into
  memory), filtered for a partial export.
- `originals/`: the files in `transactions/`.
- `settings/`: `profile.json`, `user_settings.json`, `formats/*.toml`.
- `secrets/secrets.json`: only with `--with-secrets` (or the checkbox).

**Never included:** server.db, master.key, the web app, fetch_status.json
and the locks. Only the entries above are ever added, and
`archive::tests` checks it.

**Partial exports:**

- `--from` and `--to` cut the dated tables and leave out the originals and
  the ingest records.
- `--sources` keeps those sources' rows and originals. `ibkr` keeps the
  broker tables; `hsbc_sms` keeps the fund subscriptions.

**Import modes.** Each has a dry run that reports per table what is added,
skipped and in conflict.

- **fresh:** only into an empty tree.
- **merge:**
  - natural-key `INSERT OR IGNORE` per table;
  - bank rows go through ingest's near-duplicate guard;
  - the originals are saved (same content is skipped) and re-ingested
    through the normal dedupe;
  - this tree wins on overrides, rules (matched on pattern and direction),
    settings and the broker snapshot; conflicts are listed, and
    `--prefer-archive` flips them;
  - a different base currency is refused;
  - the crypto reconcile anchors (`cash-adjust`, `asset-adjust`) are
    skipped, because the next sync derives this tree's own.
- **replace:**
  - refuses a partial archive;
  - takes a backup first (`data/backups/pre-import/`, never pruned);
  - swaps finance.db atomically and removes the WAL and SHM;
  - moves the old `transactions/` aside (`transactions.pre-import-*`).

Merge and replace hold the sync lock. Secrets in an archive go to the
encrypted store: all of them for fresh and replace, only the keys not set
here for merge.

**The backup recipient** is an age public key in server.db's meta table. It
is public, never a secret: it can only seal. The desktop app makes the key
pair (Settings, Sync & storage, Backups) and keeps the private half.

**Admin routes** (a paired admin device; writes over loopback or TLS, as
for every admin route):

| Route | What |
|---|---|
| `GET` / `PUT /api/admin/backup-recipient` | `{recipient, keep}`; the PUT validates the key, and an empty recipient turns the nightly archive off |
| `GET /api/admin/archives` | the nightly archives, newest first |
| `GET /api/admin/archives/latest` | the newest one, streamed |
| `POST /api/account/export` | `{passphrase (8+), with_secrets, from, to, sources}`; built, then streamed while it is sealed |
| `POST /api/account/import` | multipart: `file`, `passphrase` or `identity`, `mode` (default merge), `dry_run` (default **on**), `prefer_archive`. Up to 256 MB. 409 when the mode refuses (fresh into data, partial replace, another currency) |

A passphrase or identity sent to the import is used in memory for that
request only. It is never stored and never logged.

## Profile and settings

In the user's finance.db (no secrets, safe to replicate): `profile`
(base_currency, home_country, home_city, name, aliases, benchmark,
timezone, onboarding_step, avatar_kind, avatar_email, avatar_initials,
avatar_color, avatar_updated) and `user_settings` (`budgets`, `calendar`,
`filters`, `accounts`, `home` as JSON). Both ride in the bundle (`profile`,
`user_settings`, written per request, uncached). `GET|PUT /api/profile`,
`PUT /api/settings/{key}` (a device).

- `profile.base_currency` is what every amount is in (see Base currency).
  `profile.benchmark` is the Yahoo symbol the Investments page compares the
  same deposits against (default `^GSPC`, the S&P 500). `profile.home_country`
  is the prior when the data can't tell where home is (travel), and
  `home_city` draws home on the map.
- `user_settings.accounts` is `{source: {"country": "HK" | null}}`: where
  each bank or card account is (Settings → Data sources, *Account countries*).
  A source without an entry takes the source catalog's default
  (`catalog::default_country`: `hsbc`, `hsbc_card` and `mox` are Hong Kong; the
  rest none), and `null` means none: its rows are placed by their currency.
  Only set entries ride in the bundle, so an untouched tree's bundle is
  unchanged.
- `user_settings.home` is the home setup (Settings → Home):
  `{"mode": "rent" | "none", "currency", "rent": {"monthly", "landlord_match",
  "lease": {"start", "end"}}, "oneoffs": [{"kind": "deposit" | "agency_fee" |
  "stamp_duty", "amount", "date", "returned_on"}]}`, all but `mode` optional
  (`finance_core::home::HomeSetup::check` validates it). Mode `mortgage` also takes
  `mortgage: {principal, annual_rate_pct, term_months, start, payment_day,
  lender_match, home_value, home_value_date, overpayments: [{date, amount}],
  rate_changes: [{from, annual_rate_pct}]}` (principal above 0, rate 0-30, term
  1-600 months) and the one-off kind `legal_fee`.
  In mortgage mode (and only then) net worth gains a source of kind `liability`
  (`mortgage`, the schedule's balance as of today, negative) and
  `liabilities_hkd`, plus, with `home_value`, a source of kind `property`
  (`home`) and `property_hkd`; `total_hkd` and `total_eur` include both. Without
  a mortgage setup neither key exists. The month-end history is unchanged. A deposit one-off with `returned_on` makes *Owed to you* treat the
  matching held deposit (same currency, opened within 31 days of the one-off's
  date) as returned from that date; with no `home` setting the receivables are
  unchanged.

## Base currency

Every amount the views compute is in `profile.base_currency` (HKD when there
is no profile, `config::BASE_CURRENCY`). `views::fx`:

- `fx_pairs`: the currencies in the data (bank rows, the broker's positions,
  cash and trades) plus USD (crypto and the index proxies are priced in it)
  and EUR (the net worth's second figure), minus the base, each quoted in
  the base. EUR, GBP, USD and JPY come first, then the rest alphabetically,
  so HKD data with those four gives exactly the pairs the views always had.
  The fetcher (`fetch-fx`, the sync) fetches these pairs.
- `Rates` reads only rates quoted in the base, and the base is 1.
  `MonthlyRates` carries its currencies and base (`col`, `filled`).
- The bundle's `fx` section has `base` and one rate per pair.

**Names.** Bundle version 1 keeps every `_hkd` name (`total_hkd`,
`banks_hkd`, `value_hkd`, the rate columns' `eur_hkd`): they mean "in the
base currency". `total_eur` stays the EUR figure (the base itself when the
base is EUR). Planned for `BUNDLE_VERSION` 2: rename `*_hkd` to `*_base`
(`total_base`, ...), `total_eur` to `total_secondary` with a `secondary`
currency field, `costs.flexinvest` to `costs.platform`, the `flexinvest`
section to `fund`; clients read both until v1 servers are gone.

The clients (finance_core::base) take the base, the home, the benchmark,
the fund account and the account countries from each bundle: the app shows
amounts with the base's symbol (`fmt::hkd`: HK$, €, £, US$ ...) and turns
texts written for HK$/HKD and the S&P 500 into the profile's (`fmt::local`,
unchanged for HKD and the default benchmark). `server/tests/eur_base.rs`
runs a EUR household end to end.

## Valuation and receivables

Investment value is inferred, not reported: no bank export carries a
portfolio valuation. Per holding the best available mark wins: **broker
mark > index-proxy estimate > cost basis** (`engine/src/views/investments.rs`,
`holdings.rs`).

| Account `valued_by` | Valued as |
|---|---|
| `ibkr` | Broker truth: the Flex snapshot's cash and broker-marked positions, topped up with cached closes for any symbol held per the trades that the snapshot misses. History replays real fills per symbol, each marked to its own daily closes |
| `proxy` | Each contribution "buys" units of an index proxy (a USD ETF) at that day's close; the position is marked to the latest close (now) or month-end closes (history). Real units from `fund_subscriptions` win when there are any. The estimate tracks the fund's benchmark, not the fund, so it ignores the fund's costs and tracking difference |
| `cost` | Contributions net of redemptions; no fabricated gains |

**Contributions** are bank rows of an Investments-kind category, attributed
to an account by `matches` (see Investment setup). The outflow is added
back as an investment asset, so investing never reads as a drop in net
worth. The Investments view also buys the same monthly flows into the
profile's benchmark for comparison, with an annualized money-weighted
return and a time-weighted growth series.

**History vs now.** The monthly balance sheet proxy-marks month by month;
broker history stays at cost, but the running month is pinned to the broker
mark, so the trajectory's endpoint matches the net-worth tile. Cached closes
older than about 3 days trigger one refresh per symbol; offline, the cached
series is served flagged stale.

**Receivables** (`engine/src/views/receivables.rs`, the bundle's
`receivables` section) count in net worth as a `receivable` row per item,
so paying a deposit is not a drop in net worth:

- **Deposits held**: rows categorized `Deposit`, per counterparty (an
  account number in the description, else its first words) and currency.
  Outflows open a deposit, inflows refund it; a refund from a differently
  described payer closes the oldest open deposit in that currency. Balances
  never go below zero.
- **Partner balance**: from the first row with `share < 1` onward, the
  partner's part of shared bills, `(1 − share) × |amount|`, minus `Partner`
  inflows plus `Partner` outflows.
- **Reimbursements**: `Office (reimbursed)` spend, year to date and trailing 12
  months, as information only (the refunds arrive inside salary).

All month-over-month comparisons use **completed months only**: the running
month is shown as "now" but never compared.

## Investment setup

What the investment views know about the accounts and instruments is data
(`engine/src/invest_setup.rs`), made at startup
(`invest_setup::ensure`, from `auth::bootstrap`). Every tree's tables
start empty, one that predates them too (Settings → Data sources,
Investments fills them); a table that exists is left as it is, and a
missing table always reads as empty.

- **`investment_accounts`**: `name`, `matches` (a JSON array of lowercase
  fragments of the bank row's description: an Investments-kind row goes to
  the first account by `sort` whose fragment it contains, else Other),
  `valued_by` (`ibkr`: the IBKR Flex data; `proxy`: an index `proxy` in USD,
  named by `proxy_label`, bridging the real units of `fund_code` when
  fund_subscriptions has them; `cost`), and the fund platform's fees:
  `fund_ocf_pct`, `platform_flat_month` up to `platform_cliff`, then
  `platform_pct_above` of the whole balance, and `fee_match` (the
  description fragment of the fee rows, for what was paid).
- **`instruments`**: `symbol`, `name`, `kind` (`fund`, or `stock`: no
  holding fee), `ter_pct` (NULL: unknown, shown as fee unknown).

The view cache's fingerprint covers both tables and the profile's base
currency and benchmark. Routes (`server/src/investments.rs`):

| Route | What |
|---|---|
| `GET /api/investments/setup` | `{accounts, instruments, benchmark, default_benchmark, valued_by, kinds}` |
| `POST /api/admin/investments/accounts` | a new account |
| `PUT /api/admin/investments/accounts/{name}` | replace it; another name in the body renames it |
| `DELETE /api/admin/investments/accounts/{name}` | |
| `PUT /api/admin/investments/instruments/{symbol}` | `{name?, kind, ter_pct?}`, adds or changes |
| `DELETE /api/admin/investments/instruments/{symbol}` | |

The writes are a user's own (gate.rs: a write, not an admin route,
whatever the path). Every write moves `data_rev` and re-warms the views. The benchmark is the
profile's (`PUT /api/profile`). Settings → Data sources, Investments edits all of it.

**The avatar** (`avatar.rs`, routes in `accounts.rs`): `avatar_kind` is
`upload`, `gravatar` or `initials`. The picture is a file in the user's
data dir (`data/avatar/`), never in finance.db. `PUT` (or `POST`)
`/api/profile/avatar` takes a PNG, JPEG or WebP (raw body or multipart),
5 MB at most (413), sniffed by its bytes (anything else 422), decoded under
limits (8192 px a side, 256 MB), cropped to the centre square and resized to
256×256 PNG. `POST /api/profile/avatar/gravatar` {email}: the server fetches
`https://gravatar.com/avatar/<sha256 of the trimmed lowercased email>?s=256&d=404`
(clients never contact Gravatar), keeps it, and fetches it again in the
background once a day old; Gravatar's 404 answers 404 "No Gravatar for this
email." `GET /api/profile/avatar[?refresh=1]` serves the picture, 404 for
initials. `DELETE /api/profile/avatar` goes back to initials. Initials and
colour (`chart-1`..`chart-5` or `#rrggbb`) go through `PUT /api/profile`
(validated); a client that sends no `avatar_kind` leaves the avatar alone.

`POST /api/preview` (multipart like `/upload`): what each file is and how
many of its rows ingest would add, run on an in-memory copy of the db;
nothing is written. `POST /upload` answers JSON instead of the 303 when the
request accepts `application/json` (the web app).

`GET /api/version` answers `{"kind": "anser-server", "version", "bundle_version"}`
(no sign-in, nothing else in it). The desktop app's Doctor shows it.

The version is one string for every binary (`finance_core::VERSION`: also
`anser-server --version`, the MCP `serverInfo`, `/api/version`, the archive
manifest's `app_version`, and Settings and Doctor as "App" and "Server"),
stamped by `desktop/core/build.rs`: `$FINANCE_VERSION` when set at build time
(release/CI), else `git describe --tags --always --dirty` (`v0.1.0-3-gabc1234`;
before the first tag `0.1.0+gabc1234`), else the crate's version (no git).
It updates on a new commit or tag; `-dirty` is as of the last stamp.

`GET /api/sources` reports each data source for the Doctor, never a secret:
per API source (IBKR Flex, Binance, OKX) whether its keys are set (a
boolean), the last good fetch, the last attempt and its error, scrubbed of
URLs, tokens and key-like strings; per file source the newest row; for IBKR
the latest trade, the positions snapshot date, the Flex sections the last
statement carried (dividends and withholding included), a Flex error code in
plain words, and the pricing plan the commissions fit
(`finance_core::fees::infer_plan`). Fetch outcomes are logged per source in
`data/fetch_status.json` by `engine::fetch::fetch_source`, so the CLI, cron
and the server's sync all count.

## Categories and rules

Categories are data (`engine/src/categories.rs`, `engine/src/rules.rs`,
routes in `server/src/categories.rs`), in the user's finance.db so every
device and the web app share them:

- **`categories`**: name, `icon` (a Lucide name, NULL = picked from the
  name), `kind` (spending, income, transfer, investment, card_bill, fx,
  partner, deposit: every kind but spending and income is not spending),
  `fixed` (auto, always, never), `reimbursable`, `hidden` (out of the
  pickers), `sort`, `created_at`, and the flags the client pages
  read: `recurring` (strong, neutral, strict: how much evidence a recurring
  series needs), `placeless` (paid from anywhere: places nobody on a trip)
  and `obligation` (never a "maybe forgotten" charge). They replace the
  recurring finder's STRONG/STRICT lists, travel's PLACELESS list and the
  forgotten-subscription exemptions; added to an existing table with each
  row's value from those lists (data_rev doesn't move).
- **`category_rules`** gains `origin` (seed, user, suggested), `enabled`,
  `created_at`, `note`. The **effective order**: an override
  (`txn_overrides`), then the enabled user (and suggested) rules by
  (priority, id), then the enabled seed rules by (priority, id). First match
  wins. A rule's `is_transfer` follows its category's kind (old replicas
  read transfers from the rules).
- **`app_meta`**: `data_rev`, which every write below bumps. It is in
  `/api/capabilities` and in the bundle (`data_rev`, an added section).

**The migration** (`categories::ensure`) runs at startup, after a backup of
finance.db into `data/backups/pre-categories/` (once, on an existing tree).
The new columns default to user/enabled, so every existing rule becomes a
user rule and the order is the old (priority, id) one. The table is seeded
from the categories in use (rules, overrides, bank rows, and Other) with the
flags the hard-coded lists gave them, so nothing moves. Proven on a copy of
the prod db: `engine/tests/categories_migration.rs`, and `views_goldens.rs`
runs every view twice, with and without the table. A fresh install still
seeds `DEFAULT_RULES`, as seed rules; a managed table its owner emptied
stays empty. `DEFAULT_RULES` is the generic seed, without
one person's rules (an employer, family, landlords and a rent share, their
own names, a partner, an office). Those live in that person's db as user
rules; the seed only ever fills an empty table, so removing them from the
code moved nothing there (`engine/tests/owner_rules.rs` checks the owner's
cases on a copy of their db, from a fixture in their private test data).

None of it is created by `db::connect` or ingest: those leave
`sqlite_master` exactly as the base schema has it (the golden tests check
it). Every reader treats a missing table or column as before:
`categories::Flags` (what the views ask: is it a transfer, income, an
investment, a deposit, the partner, reimbursable, always or never fixed)
falls back to the old lists.

**Routes.** The GETs are reads; everything else is a write (a device under
`FINANCE_AUTH=writes`), the dry runs included (`gate.rs`).

| Route | What |
|---|---|
| `GET /api/categories` | the categories with rule and row counts |
| `POST /api/categories` | `{name, icon?, kind?, fixed?, reimbursable?, hidden?}`; flags default from the name |
| `PATCH /api/categories` | `{order: [names]}`: the manager's order |
| `PATCH /api/categories/{name}` | any flag; `name` renames |
| `POST /api/categories/{name}/merge` | `{into}` |
| `DELETE /api/categories/{name}?into=` | rows go `into` another category; into `Other`, its rules and overrides are deleted |
| `GET /api/rules` | every rule in effective order, with `rank` and `hits` (rows it decides now) |
| `POST /api/rules` | `{pattern, direction?, category, share?, priority?, note?}` |
| `PATCH /api/rules/{id}`, `DELETE /api/rules/{id}` | edit (any field, `enabled` too), delete |
| `POST /api/rules/preview` | a dry run: matches, the total in the base currency (`total_hkd`), how many would change, pinned by an override, shadowed by a stronger rule, a sample of 20 with the rule that wins each now; without a priority it gets the highest that still wins every row it matches (at most 50) |
| `POST /api/rules/test` | `{description, amount}`: the winning rule, why, the later matches, the disabled ones |
| `POST /api/rules/starter` | the starter set: `DEFAULT_RULES` without its regional patterns and shared-bill rules, as seed rules, skipping what is there |
| `PUT /api/txn/{source}/{raw_hash}/category` | `{category, share?, note?}`; `null` hands the row back to the rules |
| `GET /api/categorize/suggest` | the Other rows by merchant key, biggest spend × count first, each with a pattern and a proposal |

Names and keys in a path are %-encoded (`Utilities%20%2F%20Telecom`). A
category a rule or override names is created with its default flags.
Renames, merges and deletes rewrite the rules, the overrides and the budgets
(`user_settings.budgets`; merged amounts add up) in the same transaction.
Every write recategorizes in its transaction and replies
`{ok, result, changed: [{source, raw_hash, category, share}], data_rev,
categories, rules}`; the clients patch their copy with it
(`Ledger::apply_write`) and sync in the background. A refusal is a 400 with
the reason.

**Suggestions** (`suggest.rs`): a cluster's pattern is its merchant key
when every row's raw text has it (else its first word); a transfer's key
skips the payment words. The proposal, first that applies: the profile's
name or an alias (a self transfer), a word of a partner rule, a description
that says deposit, other rows of the same merchant that have a category, a
steady monthly or yearly series (core recurring: Subscriptions out, the
income category in), a word an existing rule uses. Accepting one is a user
rule at priority 100. `starter: true` when fewer than 20 rules are on.

## How it is wired

- Routes: `/healthz`, `/api/version`, `/api/live`, `/api/export.json`,
  `/api/ledger.json`, `/api/db.sqlite`, `/api/files`, `/api/sources`,
  `/api/capabilities`, `/api/formats`, `/app/…`, `/sw.js` (the web app's
  service worker, at the root for its scope) and `/`; `POST /upload`,
  `/sync`, `/api/overrides`, `/api/webapp`; `PUT /api/formats/{id}`; the
  account routes of `api.rs` (pairing, whoami, login, admin, sources,
  profile, settings, preview), the category and rule routes of
  `categories.rs` (see Categories and rules). Every request passes `gate.rs` first.
  Anything else is a plain-text 404, and a failing handler a plain-text 500.
- `/api/db.sqlite` is built in memory (backup API into an in-memory db,
  serialized, journal mode DELETE in the header): no plaintext copy in /tmp.
- The data requests (`/api/export.json`, `/api/ledger.json`) run auto-ingest
  first, so a file dropped in `transactions/` shows up on the next one. When
  the last ledger fetch is older than a day, they also start a background
  sync.
- `POST /upload` answers `303` with the outcome in its `Location`
  (`/upload?saved=..&skipped=..&err=..`). The desktop app's Upload page and
  `push` read it without following it.
- The MCP tools are the engine's (`engine/src/mcp/`: protocol in `mod.rs`,
  the eight tools in `tools.rs`, their schemas in `engine/assets/mcp_tools.json`)
  and read through `mcp::Source`: a connection, the market feed, the core
  ledger and the net worth. `mcp.rs` here is the live `ServerSource`
  (the blocking feed and a live valuation on stdio; the request feed and
  `live::net_worth()`, never waiting on an exchange, over HTTP; the ledger
  kept per db fingerprint), the `/mcp` route and the key API. The
  `initialize` instructions are generated (`mcp::instructions`): the banks,
  crypto venues and investment accounts this server has data for, and the
  base currency.
- Computed views are memoized process-wide on a db fingerprint
  (`engine/src/views/cache.rs`): COUNT/MAX/SUM queries over the tables
  the views read, behind a stat of the db and WAL files. Any sync, ingest, recategorization or new
  price row recomputes on the next request. Every `Ctx` under one
  fingerprint also shares the inputs (bank rows, ledgers, the sheet, ...).
  `/api/export.json` keeps everything after `net_worth` serialized and only
  writes the head per request.
- The network reaches the views only through `feed::LiveFeed`: frankfurter
  FX, Yahoo OHLC and quotes, Binance tickers (3 s bound, cached 60 s), and the
  exchange-balance sweep (one 3 s deadline). Under `FINANCE_OFFLINE=1`, the views
  get `views::Offline` instead and fall back to their caches. HTTP
  requests fetch through `REQUEST_FEED`, whose tickers are stale-while-
  revalidate: a request never waits on Binance.
- `/api/live` and the bundle's `net_worth` are the live valuation, computed by
  a background thread and cached 60 s. A request gets the cached one and, when
  it's expired, starts a refresh behind it. Before the first refresh it gets
  the snapshot valuation, flagged stale. One older than 5 minutes is served
  flagged stale.
- `serve` warms the live valuation and the bundle's views in the background
  at startup (every tenant, owner first, one at a time), after each live
  refresh, after a background sync and after an upload. A request for
  `/api/export.json` that arrives while its tenant's warm-up is running
  waits for **that pass** to finish (30 s at most), then serves from the
  cache; a later rerun of the same tenant (a live refresh, a sync) does not
  hold it. If the warm-up panics, waiters are released and compute
  themselves. A request only waits on its own tenant.

## Format definitions

Bank exports beyond the built-in parsers are described by TOML **format
definitions** (see `desktop/docs/formats.md`): the built-in ones plus
`$FINANCE_HOME/formats/*.toml` (`$FINANCE_FORMATS_DIR`). `/upload` and ingest
take `.xlsx` and `.xls` as well as `.csv`, `.xml` and `.pdf`, with the same
rules. `GET /api/formats` lists the definitions in effect, each with its TOML,
plus any file that failed to load. `PUT /api/formats/{id}` installs one. The
body is the TOML (64 KB at most), and its `id` must match the path. It is
validated before it is written. The request must carry `X-Finance-Format: 1`,
the same guard as `/api/webapp`; it is a write like `/upload` (gate.rs), and
reachable only on the tailnet. After the save, transactions/ is ingested again, so
files that only the new definition reads are picked up.

## The web app

`data/webapp/` holds the browser build of the desktop app
(`desktop/scripts/build-web.sh`). `GET /app/<file>` serves it (a `.gz`
sibling when the client takes gzip; ETag + `no-cache`), and `/` serves its
`index.html`. Until a bundle is installed, `/` and `/app/` answer a short page
saying how to install it: the desktop app's Doctor (*Install web app on the
server*), or `desktop/scripts/build-web.sh --install URL`.
`GET /api/ledger.json` is every ledger row as `finance_core::Ledger` JSON,
gzipped when accepted. `GET /api/demo/ledger.json` and
`/api/demo/export.json` are the same two documents for the made-up
household of the app's *Just try it* (`finance_engine::demo`): public, built
once a day in a scratch folder as a throwaway tenant, from nothing of any
user's (`src/demo.rs`). `POST /api/webapp` installs a bundle: multipart
(`files` field, like `/upload`) with `X-Finance-Install: 1`, which a
cross-site form can't send without a preflight this server never answers;
like `/upload` it is reachable only on the tailnet. The files go to
`data/webapp.new/` and replace `data/webapp/` by rename.

## Local first, push when reachable

The same binary runs on the laptop against the local tree. With
`serve --push-to https://your-server…ts.net` (or `anser-server push` from cron), it
checks the remote's `/healthz`. If the remote answers, it asks `/api/files`
for the content hashes and override keys the remote already has. It then
uploads only the missing transaction files (`POST /upload`, which ingests
them) and the missing overrides (`POST /api/overrides`). Every step is
idempotent on the remote: uploads dedupe by name and content, ingest dedupes
rows by natural key, and overrides are `INSERT OR IGNORE`, so the remote's
own choice wins. When the remote is unreachable, `push` exits cleanly.
Remote names resolve through `finance_core::tailnet`, so `.ts.net` works even
when a VPN owns the system DNS.

## Golden and identity tests

`cargo test --workspace` (from `desktop/`) runs everything. Most tests use
made-up data and run anywhere. The ones that prove "the owner's numbers
don't move" need the owner's private snapshot, which stays gitignored under
`data/parity/`: `prod.db`, `transactions/`, the view goldens frozen at the
2026-10-03 cutover, `bundle-hkd.json`, `client-hkd.txt` and
`owner_rules.json`. They read `$FINANCE_PARITY_DIR`, work on scratch copies
(the gitignored `data/test-tmp/` or the temp dir), and skip cleanly when it
is unset or absent. Nothing derived from it is ever committed.

```bash
cd desktop
cargo test --workspace                                            # skips the owner's checks
FINANCE_PARITY_DIR=$PWD/../data/parity cargo test --workspace     # runs them too
```

On the owner's snapshot (opt-in, with `FINANCE_TODAY=2026-10-03` and
`FINANCE_OFFLINE=1` where the views run):

- `engine/tests/bundle_identity.rs`: "the same experience". On a copy of
  prod.db with the startup migrations and the owner's profile (HKD, HK),
  `export::bundle_bytes` must equal `bundle-hkd.json` byte for byte, the
  wall-clock fields (`generated_at`, the now-stamped `as_of`) blanked.
  `FINANCE_BLESS_BUNDLE=1` writes the golden; bless only from a known-good
  commit.
- `server/tests/client_identity.rs`: the same for what the app computes from
  the replica (the ledger's base amounts, trips and routes, recurring series
  and flags, budget suggestions) against `client-hkd.txt`
  (`FINANCE_BLESS_CLIENT=1`).
- `engine/tests/views_goldens.rs`: every bundle section and three
  `month_detail`s against the frozen view goldens, with readable per-field
  diffs. Keys, key order and strings exact, cents within 0.01, other numbers
  within 1e-9 relative. It runs twice: on the db as is, and after the
  categories migration.
- `engine/tests/categories_migration.rs`: the migration leaves every row's
  category and share, the rule order and every flag answer as they were.
- `engine/tests/owner_rules.rs`: the owner's personal rules, from
  `owner_rules.json`, still decide their rows, and none of them comes from
  the code.
- `engine/tests/ingest_parity.rs`: re-ingesting the real exports onto prod
  adds nothing; re-runs, touched files, copies, overlapping slices and any
  file order add nothing either.
- `engine/tests/preview_parity.rs`: every real file previews 0 new rows
  against a replica (the Upload page's dry run).
- `engine/tests/archive_parity.rs`: export, verify, fresh import and merges
  of a `.finance` archive at the owner's scale.
- `server/tests/owner_migration.rs`: the owner's first start of an
  auth-aware server changes nothing but the added tables and columns.
- `core/tests/replica_e2e.rs`: the encrypted replica through the app's own
  readers.

Data-free, everywhere:

- `engine/tests/ingest_dedupe.rs`: a made-up tree with every dedupe hard
  case; each scenario adds 0 rows, and the db it builds is pinned by
  per-table digests (a change would orphan or re-insert stored rows).
- `engine/tests/ingest_numfmt.rs`: the number formats the ingest key embeds,
  pinned over ~270k values.
- `server/tests/server_routes.rs`: the HTTP surface on a throwaway tree.
- `server/tests/category_routes.rs`: the category and rule routes end to end
  (migration, override, preview, rule edit and test, rename, merge and
  delete with budgets, suggestions, the starter set, data_rev).
- `server/tests/isolation.rs`: every route called as another user.
- `server/tests/eur_base.rs`: a EUR household's pairs, bundle and
  investment setup routes.
- `server/tests/mcp.rs`: the MCP protocol (versions, batches, validation
  errors) and every tool on the demo household, with the totals that must
  tie out (breakdown = summary = the spending page, holdings = the net
  worth, performance = holdings, the broker's exact figures), and the warm
  p50 per tool.
- `server/tests/mcp_http.rs`: `/mcp` and its keys end to end (the switch,
  who may call it, what a key may not reach, one user's key against
  another's data) and the `mcp-key` / `mcp-enable` commands.
