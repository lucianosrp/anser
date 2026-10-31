//! Accounts and secrets from the shell (on the server's machine):
//!
//! * `anser-server pair`: a one-time 8-character code, valid 10 minutes,
//!   printed and written to `$FINANCE_HOME/secrets/pair-code` (0600). Enter
//!   it in the desktop app (Settings or the Doctor's Server row), or the
//!   browser's /login page.
//!   On a fresh install with no account yet, this makes the owner (user 1,
//!   admin) first: whoever has the server's shell owns it. `--user NAME|ID`
//!   makes the code for another user's device.
//! * `anser-server user list`: users and their devices.
//! * `anser-server user add NAME [--email E] [--admin]`: an account with its
//!   own tree (users/<id>/), no invite needed on the server's shell.
//! * `anser-server user rename NAME|ID NEW_NAME`.
//! * `anser-server user remove NAME|ID [--purge --yes-delete-their-data]`:
//!   the account and its devices go; the tree and secrets stay on disk
//!   unless purged (which needs the confirmation flag). User 1 stays.
//! * `anser-server user revoke DEVICE_ID`.
//! * `anser-server auth [off|writes|all]`: show or set the stored auth mode
//!   (FINANCE_AUTH in the environment still wins).
//! * `anser-server secrets import-env`: move the source keys from .env into
//!   the encrypted store (.env.bak keeps the old file).

use anyhow::{bail, Result};

use finance_engine::secrets;
use crate::auth::{self, Mode};

fn when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

pub fn pair(args: &[String]) -> Result<()> {
    super::args::no_args("pair", args);
    super::serve::private_umask();
    auth::bootstrap()?;
    // a fresh install nobody has claimed from the app: the shell's user owns it
    auth::ensure_owner()?;
    let (code, expires) = auth::new_pair_code(secrets::active_user())?;
    let file = secrets::dir()?.join("pair-code");
    secrets::write_private(&file, format!("{code}\n").as_bytes())?;
    let uid = secrets::active_user();
    let whose = if uid == finance_engine::tenant::OWNER_UID { String::new() } else { format!(" for user {uid}") };
    println!("Pairing code{whose}: {}-{}", &code[..4], &code[4..]);
    println!("Valid until {} (10 minutes), for one device. Also in {}", when(expires), file.display());
    println!("Enter it in the desktop app (Settings, or Doctor > Server) or at <server>/login in a browser.");
    Ok(())
}

pub fn user(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("list") | None => {
            auth::bootstrap()?;
            println!("auth mode: {}{}", auth::mode().as_str(), if auth::mode_from_env() { " (FINANCE_AUTH)" } else { "" });
            println!("new accounts: {}", match auth::signup_mode() {
                "first" => "none yet: the first account made in the app becomes the admin",
                "open" => "anyone who reaches the server (FINANCE_ALLOW_SIGNUP)",
                _ => "by invite (Settings, Admin, Invite someone)",
            });
            if auth::household() {
                println!("household server: every user sees only their own tree; reads need a device (auth all)");
            }
            for u in auth::users()? {
                let home = if u.home.is_empty() { "none yet (made at the next start)" } else { u.home.as_str() };
                println!("user {} {} ({}), home {home}, {} device(s), since {}", u.id, u.name, u.role, u.devices, when(u.created));
            }
            for d in auth::devices()? {
                println!("  device {} {:<28} user {}  paired {}  last seen {}", d.id, d.name, d.user_id, when(d.created),
                    d.last_seen.map(when).unwrap_or_else(|| "never".into()));
            }
            Ok(())
        }
        Some("add") => {
            let name = args.get(1).filter(|n| !n.starts_with("--")).ok_or_else(|| anyhow::anyhow!("usage: {} {USER_USAGE}", super::prog()))?;
            let a = super::args::Args::parse(&args[2..], &["--email"], &["--admin"])?;
            super::serve::private_umask();
            auth::bootstrap()?;
            let id = auth::add_user(name, a.value("--email").as_deref(), a.has("--admin"))?;
            let u = auth::find_user(&id.to_string())?;
            println!("user {id} {} ({}), home {}", u.name, u.role, u.home);
            println!("Pair their first device: {} pair --user {id}", super::prog());
            Ok(())
        }
        Some("rename") => {
            let (Some(key), Some(name)) = (args.get(1), args.get(2)) else { bail!("usage: {} {USER_USAGE}", super::prog()) };
            let u = auth::find_user(key)?;
            auth::rename_user(u.id, name)?;
            println!("user {} is now {}", u.id, name.trim());
            Ok(())
        }
        Some("remove") => {
            let a = super::args::Args::parse(args.get(2..).unwrap_or(&[]), &[], &["--purge", "--yes-delete-their-data"])?;
            let Some(key) = args.get(1) else { bail!("usage: {} {USER_USAGE}", super::prog()) };
            let u = auth::find_user(key)?;
            let purge = a.has("--purge");
            if purge && !a.has("--yes-delete-their-data") {
                bail!("--purge deletes {} and user {}'s secrets for good: add --yes-delete-their-data to confirm", u.home, u.id);
            }
            let r = auth::remove_user(u.id, purge)?;
            println!("user {} {} removed, {} device(s) revoked", u.id, u.name, r.devices);
            if r.purged {
                println!("their tree {} and secrets were deleted", r.home);
            } else if !r.home.is_empty() {
                println!("their tree stays at {} (and secrets/{}.json.enc): delete them by hand, or run remove again with --purge", r.home, u.id);
            }
            Ok(())
        }
        Some("revoke") => {
            let id: i64 = args.get(1).and_then(|s| s.parse().ok()).ok_or_else(|| anyhow::anyhow!("usage: {} user revoke DEVICE_ID", super::prog()))?;
            println!("{}", if auth::revoke(id)? { "revoked" } else { "no such device" });
            Ok(())
        }
        Some(other) => bail!("usage: {} {USER_USAGE}  (unknown: {other})", super::prog()),
    }
}

const USER_USAGE: &str = "user [list | add NAME [--email E] [--admin] | rename NAME|ID NEW_NAME | \
remove NAME|ID [--purge --yes-delete-their-data] | revoke DEVICE_ID]";

pub fn auth_mode(args: &[String]) -> Result<()> {
    auth::bootstrap()?;
    match args.first() {
        None => println!("{}", auth::mode().as_str()),
        Some(m) => {
            let m = Mode::parse(m).ok_or_else(|| anyhow::anyhow!("usage: {} auth [off|writes|all]", super::prog()))?;
            auth::set_mode(m)?;
            println!("auth mode stored: {} (restart not needed{})", m.as_str(),
                if auth::mode_from_env() { "; FINANCE_AUTH in the environment still overrides it" } else { "" });
        }
    }
    Ok(())
}

pub fn secrets_cmd(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("import-env") => {
            super::serve::private_umask();
            let keys = secrets::import_env(secrets::active_user())?;
            if keys.is_empty() {
                println!("nothing to import: no source keys set in .env");
            } else {
                println!("imported {} into {} (.env.bak keeps the old .env; the lines are commented out in .env)",
                    keys.join(", "), secrets::store_path(secrets::active_user())?.display());
            }
            Ok(())
        }
        Some("status") | None => {
            for k in secrets::SECRET_KEYS {
                println!("{k:<20} {}", secrets::configured_via(k).map(|v| v.as_str()).unwrap_or("not set"));
            }
            Ok(())
        }
        Some(other) => bail!("usage: {} secrets [status|import-env]  (unknown: {other})", super::prog()),
    }
}
