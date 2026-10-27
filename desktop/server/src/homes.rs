//! Per-user data trees (finance_engine::tenant has the layout): making
//! one when an account is made, and purging one when an admin removes a
//! user with `--purge`.
//!
//! A new tree is what a fresh install has after its first account: the
//! schema, the categories and investment tables (empty: none of the
//! owner's accounts, instruments or rules), a profile with the user's name
//! and no settings; the onboarding wizard does the rest. The owner's tree
//! is never made or purged here: it is $FINANCE_HOME, in place.

use std::path::Path;

use anyhow::{bail, Context, Result};

use finance_engine::tenant::{self, Tenant};
use finance_engine::{categories, config, db, invest_setup, profile};

#[cfg(unix)]
fn private_dir(d: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn private_dir(d: &Path) -> Result<()> {
    std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    Ok(())
}

/// Make (or complete: idempotent) user `uid`'s tree under
/// `$FINANCE_HOME/users/<uid>` and return its tenant. `name` seeds the
/// profile when it has none.
pub fn init(uid: i64, name: &str) -> Result<Tenant> {
    if uid == tenant::OWNER_UID {
        bail!("user 1 is the owner: their tree is $FINANCE_HOME");
    }
    let t = Tenant::user_at(uid, &Tenant::default_home(uid));
    private_dir(&config::root_home().join("users"))?;
    for d in [&t.home, &t.data_dir, &t.transactions, &t.formats, &t.avatar] {
        private_dir(d)?;
    }
    t.enter(|| -> Result<()> {
        let conn = db::open()?;
        profile::ensure(&conn)?;
        categories::ensure(&conn)?;
        invest_setup::ensure(&conn)?;
        if profile::get(&conn)?.is_none() {
            profile::put(&conn, &profile::Profile {
                base_currency: config::NEW_PROFILE_CURRENCY.into(),
                name: Some(name.trim().to_string()).filter(|n| !n.is_empty()),
                ..Default::default()
            })?;
        }
        Ok(())
    })?;
    Ok(t)
}

/// Delete user `uid`'s tree. Only a users/<uid> directory of this server
/// is ever removed (never $FINANCE_HOME); the secrets file is the caller's
/// (finance_engine::secrets::purge).
pub fn purge(uid: i64, home: &str) -> Result<bool> {
    let Some(t) = Tenant::for_user(uid, home) else { return Ok(false) };
    if t.legacy || uid == tenant::OWNER_UID {
        bail!("refusing to delete the owner's tree");
    }
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let users = canon(&config::root_home().join("users"));
    let h = canon(&t.home);
    if !h.starts_with(&users) || h == users {
        bail!("{} is not a user tree of this server", t.home.display());
    }
    match std::fs::remove_dir_all(&h) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", h.display())),
    }
}
