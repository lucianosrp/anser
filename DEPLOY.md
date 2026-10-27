# Deploying to your server

The server runs on any Linux machine you own that has systemd user units and
Tailscale: a Raspberry Pi (the reference setup, see
[On a Raspberry Pi](#on-a-raspberry-pi)), a home server, a small VPS. Its
checkout lives at `~/finance`; the laptop checkout is the dev copy. The
server (`anser-server`, installed at `~/.local/bin/anser-server`) listens on
`127.0.0.1:8787` only, and `tailscale serve` publishes it to the tailnet.
The syncs and the SQLite databases all live on the server.

The browser UI is the web app: the desktop app's pages built for wasm,
served at the server root (`/`, also `/app/`). The server renders no HTML
pages of its own. A tagged deploy installs the web app from the release;
`--build` leaves it as today (see [The web app](#the-web-app)).

The examples below use `ssh your-server`: an ssh destination (a host
alias in `~/.ssh/config`, or `user@host`) for that machine.

## Deploy

Tell the script where the server is, once:

```bash
cp scripts/deploy.local.example scripts/deploy.local   # gitignored
# edit it: FINANCE_HOST="your-server your-server-tailnet"
```

`FINANCE_HOST` takes one or more ssh destinations separated by spaces; the
first that answers wins (say, an alias on the home LAN, then one over
Tailscale for when you're away). `deploy.local` may also set `REMOTE_DIR`,
`PORT` and `BRANCH`. `FINANCE_HOST=… scripts/deploy.sh` in the environment
overrides the file. Then:

```bash
scripts/deploy.sh
```

This ships **committed** `main` and does the following:

1. Picks the first reachable host from `FINANCE_HOST`.
2. Bundles the branch and its tags (`git bundle`), copies it over with
   `scp`, runs `git merge --ff-only` on the server, then fetches the tags
   (so the server's `git describe` names a `--build` build; `/api/version`).
   The server needs no key for the git remote, so it never runs `git pull`.
   If the server's checkout has diverged, the merge aborts rather than
   overwriting anything.
3. Installs `anser-server`. If HEAD is a `v*` tag (or you pass
   `--release vX.Y.Z`), the server downloads that release's
   `anser-server-<arch>-linux.tar.xz`, `anser-web.tar.xz` and `SHA256SUMS`
   in one go, verifies the checksums, installs the binary to
   `~/.local/bin/anser-server` and unpacks the web app into `data/webapp`
   (atomic swaps: `webapp.new` → `webapp`). If that release is not up yet, it
   stops and tells you to wait or use `--build`. Otherwise (and with
   `--build`) it compiles on the server (`cargo build --release -p
   finance-server` in `~/finance/desktop`), installs the binary, then runs
   `cargo clean` (small disks can't spare the ~1-2 GB of `target/`;
   `KEEP_TARGET=1` keeps the build dir for faster redeploys). `--build` does
   not install the web app.
4. Runs `anser-server backup`, restarts `anser-server` and polls `/healthz`. If
   the health check fails, it prints the last 30 log lines.

Other modes:

```bash
scripts/deploy.sh --restart         # restart only, no code shipped
scripts/deploy.sh --install-units   # (re)install the systemd units, then deploy
scripts/deploy.sh --build           # compile on the server even if HEAD is tagged
scripts/deploy.sh --release vX.Y.Z  # take that GitHub release's binaries
```

A first checkout at `~/finance` (`git clone`, or a `git bundle` copied
over) is still required so docs and units stay in sync. `--build` also
needs a Rust toolchain on the server (`rustup`, on the login shell's PATH
through `~/.cargo/env`).

### On a Raspberry Pi

The reference setup is a Raspberry Pi (aarch64, Raspberry Pi OS) on the home
LAN and the tailnet.

- **Tagged deploys skip the compile.** A `v*` tag on HEAD downloads the
  aarch64 `anser-server` tarball and the web app on the Pi. `--build` is the
  old path: the first `cargo build --release -p finance-server` takes 20+
  min with little output; since every build deploy cleans `target/`, each
  one rebuilds from scratch. `KEEP_TARGET=1 scripts/deploy.sh --build`
  keeps the build dir while iterating, if the card has room.
- **Disk.** An SD card is small: the `cargo clean` after each `--build` is
  why those deploys don't fill it. Check with `df -h /` (the script prints
  it).
- **Memory.** A Pi with little RAM may need swap for a `--build`.
- **Two routes.** An mDNS alias for the LAN (`raspberrypi.local`) and a
  Tailscale one for away, both in `FINANCE_HOST`, so the script picks
  whichever answers.
- **Backups share the card** with the database (see [Backups](#backups)):
  copy them off now and then.

## Releases

Push a `v*` tag on `main` (or run the Release workflow and pass that tag):

```bash
git checkout main
git tag v0.1.0
git push origin v0.1.0
```

`.github/workflows/release.yml` attaches:

- `anser-server-x86_64-linux.tar.xz` and `anser-server-aarch64-linux.tar.xz` — the server. aarch64 is cross-compiled on Ubuntu 22.04 (glibc 2.35), so it runs on Raspberry Pi OS bookworm (glibc 2.36).
- `anser-x86_64-linux.tar.xz` — the GPUI app (Ubuntu 24.04).
- `anser-web.tar.xz` — the wasm bundle (`desktop/scripts/build-web.sh`).
- `SHA256SUMS`

The desktop app updates itself from the latest release: it offers (never installs unasked) a newer `vX.Y.Z`, and *Update* in Settings verifies the tarball against `SHA256SUMS` before swapping the binary. So a tag needs those two assets, and prereleases and `-rc` tags are never offered.

On a laptop or the server itself:

```bash
curl -fsSL https://raw.githubusercontent.com/lucianosrp/anser/main/install.sh | sh
curl -fsSL https://raw.githubusercontent.com/lucianosrp/anser/main/install.sh | sh -s -- --server
```

`install.sh` verifies each tarball against `SHA256SUMS` and writes into
`~/.local/bin` (no sudo). `--server` prints the pairing / systemd /
`tailscale serve` next steps; it does not enable units.

`scripts/deploy.sh` installs the same assets when HEAD is that tag. `FINANCE_RELEASE_BASE` points the download at another URL or directory (a testing aid).

## Service (systemd user units)

The unit files live in `deploy/` and are installed to
`~/.config/systemd/user/` by `--install-units`. Linger is enabled for the
user, so the units start at boot.

| Unit | What |
|---|---|
| `anser-server.service` | `anser-server serve` on `127.0.0.1:8787`, `FINANCE_HOME=%h/finance` (this checkout-based setup; a bare install defaults to `~/.local/share/finance`). `Restart=always` |
| `anser-backup.timer` → `anser-backup.service` | daily 04:15 `anser-server backup --keep 14`. `Persistent=true`, so a missed run happens at the next boot |

```bash
systemctl --user status anser-server
journalctl --user -u anser-server -f                       # live log
journalctl --user -u anser-server --since today | grep -i error
systemctl --user list-timers anser-backup.timer
```

A failing handler answers a plain-text 500 and logs why; a page in the app
never depends on it (cached data and a stale badge).

`data/parity/` on the laptop is the owner's private test snapshot (see
README, Tests); it is not needed on the server.

## Devices and accounts

`FINANCE_AUTH`, when set, wins. Otherwise the default is decided once, at the
first start, and stored: `all` (every request needs a paired device or
session), for a new install and for a tree that already has a `finance.db`
alike. Run `anser-server auth writes` to let the tailnet read without a token
(uploads, settings and pushes still need a paired device); a mode already
stored is never changed by an upgrade. A household server is always `all`. On its first start an
existing tree is backed up to `data/backups/pre-auth/`; the server makes
`data/server.db` (the first account is user 1, admin) and seeds the profile.

1. **Pair the laptop.** `ssh your-server '~/.local/bin/anser-server pair'` prints a
   code (valid 10 minutes, one device). In the desktop app: Settings,
   Account, Device: enter the code (or the Doctor's Server row, *Pair this
   device*). The token goes to the OS keyring.
2. **Pair the phone.** On the laptop: Settings, Account, *Pair another device*.
   On the phone, open the web app: when the server turns it away it opens
   on its sign-in screen; *Do you have a pairing code?* Yes, enter it. The
   browser keeps a session cookie. (`/login` is a plain fallback page for a
   browser the web app can't run in.)
3. **Keys.** Settings, Data sources, *Edit* on a source: the server tests the key
   (read-only, it works) and stores it encrypted in
   `~/finance/secrets/<uid>.json.enc` (master key `secrets/master.key`,
   0600). Keys still in `.env` move over with Settings, Admin, *Import keys
   from .env* (or `anser-server secrets import-env`), which keeps `.env.bak`;
   delete it once a sync has worked.
4. **A laptop server that pushes** (`serve --push-to`): pair it too, or
   set `FINANCE_TOKEN` for it, or its pushes get 401.
5. **Lock reads, last.** With every device paired: Settings, Admin, *Lock
   reads* (or `anser-server auth all`). From then on `/api/db.sqlite`, the
   export and the ledger answer paired devices only. Until then the Doctor
   warns that reads are open to every tailnet device.

**Other people.** Settings, Admin, *Invite someone* makes a one-time invite
(24 hours, one account); the invitee picks *No, create a new account* on
the sign-in screen. Self-signup without an invite stays off unless
`FINANCE_ALLOW_SIGNUP=1` is set for the service. A brand-new install (no
finance.db) has no users: the first account made in the app becomes the
admin, so claim it right after the first start (or run `anser-server pair`
on the server, which claims it for you).

Back up `secrets/master.key` somewhere off the server (a password manager): the
store can't be opened without it, and it is in no backup.

## A household: several users on one server

Each person on the server has their own data: their own SQLite db, files,
sources and keys. The owner is user 1 at `~/finance`. Everyone else lives
under `~/finance/users/<id>/` (`data/finance.db`, `transactions/`,
`formats/`, `avatar/`), their keys in `~/finance/secrets/<id>.json.enc`
(the same master key).

- **Add someone**: Settings, Admin, *Invite someone* (they sign up on the
  sign-in screen), or on the server
  `~/.local/bin/anser-server user add Sam --email sam@example.com`, then
  `~/.local/bin/anser-server pair --user Sam` for their first device. Their
  first sign-in opens the setup wizard on their own, empty tree.
- **Reads lock themselves.** With a second user, the server answers data
  only to paired devices, whatever `FINANCE_AUTH` says (the log and
  `anser-server user list` say "household server"). Pair every device first.
- **Their syncs** run on their own: every 15 minutes the server syncs each
  user whose ledger is a day old and who has a source set up, at most two
  at a time. `journalctl --user -u anser-server | grep 'sync user'`.
- **Backups**: the nightly `anser-server backup` backs the owner's db up,
  then each user's into their own `data/backups/`; each user seals their
  nightly archive to their own backup key (Settings, Sync & storage, Backups).
- **Managing users**: Settings, Admin, Users (accounts and their devices;
  removing an account removes its devices and the account, its data stays
  on the server), or `anser-server user list | rename | remove`. Deleting
  someone's data is shell-only and asks twice:
  `anser-server user remove Sam --purge --yes-delete-their-data`.
- **What an admin can see.** Through the app and the API, nobody reads
  anyone else's data, admins included (no impersonation; the isolation
  test calls every route as another user). On the server itself, whoever has
  your shell can read every user's files, and with `secrets/master.key`
  their keys: tell household members so.
- **The desktop app with two accounts**: the avatar (top left) opens the
  account menu: *Add an account…* (this server or another), switch, *Sign
  out…*. Each account keeps its own local copy and token under
  `~/.local/share/finance-desktop/profiles/<account>/`.

## The web app

A tagged `scripts/deploy.sh` installs `anser-web.tar.xz` into
`data/webapp/` (same atomic swap as `POST /api/webapp`: `webapp.new` →
`webapp`). `--build` does not; install or update it from the laptop (the
server doesn't need the wasm toolchain):

- the desktop app: Doctor, then *Install web app on the server* (*Update*
  once installed). It builds this checkout, installs it on the configured
  server and shows the build log; or
- `desktop/scripts/build-web.sh --install https://your-server.<tailnet>.ts.net` from a
  checkout.

Both need `rustup target add wasm32-unknown-unknown` and
`cargo install wasm-bindgen-cli --version <Cargo.lock's wasm-bindgen>`
once. Until a bundle is installed, `/` answers a short page saying how to
install it. The API (`/api/export.json`, `/api/ledger.json`, ...) answers
either way. The service worker offers a new build with *Update available*.

## After rotating an API token

Settings, Data sources, *Edit* on the source, paste the new key, *Save*: the
server tests it and stores it encrypted; the next fetch uses it, no
restart. A key still in `.env` is re-read when the file changes.

## Backups

```bash
ssh your-server '~/.local/bin/anser-server backup'                       # ad hoc
ssh your-server 'ls -lh ~/finance/data/backups/'
scp your-server:finance/data/backups/finance-YYYYMMDD-HHMMSS.db data/ # off-device copy
```

`anser-server backup` uses the SQLite online-backup API, which is consistent
even while a sync is writing. Don't use a plain `cp` on a WAL database. Only
the newest `finance-*.db` files in `data/backups/` are kept. Hand-made
backups elsewhere in `data/` are never pruned. The server needs no `sqlite3`
CLI: copy a backup to the laptop to inspect it (read-only).

Backups live on the same disk as the db. They protect against a bad
migration or ingest, not against losing the disk. Copy one off-device now
and then, or use the encrypted archives below.

**Restore:** stop the service, copy the backup over `data/finance.db`,
delete `data/finance.db-wal` and `data/finance.db-shm`, then start the
service again.

### Encrypted off-device backups (optional)

1. **Enable:** in the desktop app, Settings, Sync & storage, *Generate backup key*.
   The laptop keeps the private key (the OS keyring) and sends the public
   key to the server. From the next timer run on, `anser-server backup` also writes
   `data/backups/archives/finance-YYYYMMDD-HHMMSS.finance`, sealed to that
   key, keeping the newest 14. Then *Save a copy of the key…* somewhere off
   the laptop, such as a password manager: without it the archives can't be
   opened. Check it ran:
   `ssh your-server 'ls -lh ~/finance/data/backups/archives/'`.
2. **Fetch:** Settings, Sync & storage, *Fetch latest backup* saves the newest one
   to `~/Backups/finance/archives/`. From a shell:
   `scp your-server:finance/data/backups/archives/finance-*.finance ~/Backups/finance/archives/`.
3. **Check one:** Settings, Sync & storage, *Verify a backup…*, or
   `anser-server verify FILE --identity finance-backup-key.txt`.

A passphrase archive any time: Settings, Sync & storage, *Export…* (the web app
has it too), or
`ssh your-server '~/.local/bin/anser-server export --out /tmp/x.finance --passphrase-file -'`.

**Restore from an archive.** Every import takes a dry run first.

- **Onto the server, replacing its data:** Settings, Sync & storage, Import:
  *Replace*, *Check first*, then *Import*. Or on the server:

  ```
  anser-server import FILE --mode replace --identity KEYFILE --dry-run
  anser-server import FILE --mode replace --identity KEYFILE
  ```

  It backs finance.db up to `data/backups/pre-import/` first, swaps the
  database atomically, moves the old `transactions/` aside and takes the
  sync lock. No restart is needed.
- **Onto a new install** (an empty tree): `--mode fresh`.
- **Into a tree that has data you want to keep:** `--mode merge`. It adds
  what is missing and keeps the tree's own overrides, rules and settings
  (`--prefer-archive` lets the archive win).

`server.db` (devices) and `secrets/master.key` are never in an archive. Back
the master key up separately, as above. After restoring onto a new server, pair
the devices again.

## MCP for an assistant

An AI assistant reads your finances through `/mcp`, the server's MCP
endpoint (read-only tools; the list is in
[desktop/server/README.md](desktop/server/README.md#mcp)). It is part of the
same server and so of the same exposure: it listens on `127.0.0.1:8787`
and `tailscale serve` publishes it to your tailnet, nowhere else. Do **not**
use `tailscale funnel` or any public proxy for it: a key is a password.
Only a client on your tailnet can connect, with a key.

Per user, on the server (`--user NAME|ID` for a household member; without it
the owner):

```bash
anser-server mcp-enable                    # off by default (no restart needed); mcp-disable closes it
anser-server mcp-key create "Claude Code"  # prints the key once: fin_...; keep it like a password
anser-server mcp-key list                  # names, when made, when last used (never the key)
anser-server mcp-key revoke ID             # the key stops working at once
```

The same from the app's Settings (it calls `GET|PUT /api/mcp`, `POST
/api/mcp/keys`, `DELETE /api/mcp/keys/{id}`). An MCP key opens `/mcp` and
nothing else: any other route answers it 403, so a key left in a config file
can't read the export bundle, change a setting or make another key. A request
with a browser session cookie on `/mcp` is refused, and so is one from a
page on another origin. One key per client makes revoking one safe.

Connect a client to `https://YOUR-SERVER.YOUR-TAILNET.ts.net/mcp` (the name
`tailscale serve` shows), with the key as a bearer token:

```bash
# Claude Code
claude mcp add --transport http anser https://HOST/mcp --header "Authorization: Bearer KEY"
```

```jsonc
// Claude Desktop: claude_desktop_config.json (through the mcp-remote bridge)
{"mcpServers": {"anser": {"command": "npx",
  "args": ["-y", "mcp-remote", "https://HOST/mcp", "--header", "Authorization: Bearer ${FINANCE_MCP_KEY}"],
  "env": {"FINANCE_MCP_KEY": "KEY"}}}}

// Cursor: ~/.cursor/mcp.json
{"mcpServers": {"anser": {"url": "https://HOST/mcp", "headers": {"Authorization": "Bearer KEY"}}}}
```

```toml
# Codex: ~/.codex/config.toml
[mcp_servers.anser]
url = "https://HOST/mcp"
bearer_token_env_var = "FINANCE_MCP_KEY"   # export FINANCE_MCP_KEY=KEY
```

(`HOST` and `KEY` are placeholders. Claude Desktop's custom connectors are
called from Anthropic's cloud, which can't reach a tailnet, and this
endpoint speaks no OAuth: use the bridge above, which runs on your machine.)
Check it from a shell:

```bash
curl -s https://HOST/mcp -H "Authorization: Bearer KEY" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

404 means the user hasn't run `mcp-enable`; 401 a missing, wrong or revoked
key; 403 a browser cookie, an ordinary device token, or an Origin or Host the server doesn't know
(`FINANCE_ALLOWED_HOSTS` adds a name).

**Local mode** needs no server, key or network: the desktop app's stdio MCP
server reads this device's copy (as fresh as the last sync). Settings,
Assistants shows the command when no server answers; by hand:

```bash
claude mcp add anser-local -- /path/to/anser mcp   # [--account NAME] for another account
# Claude Desktop / Cursor: {"mcpServers": {"anser-local": {"command": "/path/to/anser", "args": ["mcp"]}}}
```

## Gotchas

- `pkill -f "anser-server serve"` inside an ssh one-liner also matches the
  ssh shell's own command line and kills it. Use
  `pkill -f 'bin/[a]nser-server serve'`: the bracket still matches the server
  but not the literal text of the command. With the unit installed, just use
  `systemctl --user restart anser-server`.
- A stdio MCP client started over ssh (`anser-server mcp`) that died without
  closing stdio can leave a process behind:
  `pgrep -af 'bin/[a]nser-server mcp'` lists them. The HTTP endpoint
  (`/mcp`, above) has no such process.
