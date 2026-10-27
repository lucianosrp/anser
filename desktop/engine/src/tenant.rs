//! Whose data: the tenant a piece of work runs for.
//!
//! A household server keeps one data tree per user. The owner's tree is
//! the legacy layout, in place (user 1: `$FINANCE_HOME` itself, with every
//! FINANCE_DB / FINANCE_DATA_DIR / FINANCE_TRANSACTIONS_DIR /
//! FINANCE_FORMATS_DIR override still honoured); every other user's is
//! `$FINANCE_HOME/users/<uid>/`:
//!
//! ```text
//! users/<uid>/data/finance.db   the db (fetch_status.json, sync.lock,
//!                               backups/ beside it)
//! users/<uid>/transactions/     dropped and uploaded files
//! users/<uid>/formats/          the user's format definitions
//! users/<uid>/avatar/           the profile picture
//! ```
//!
//! and the secrets of every user sit side by side in the shared
//! `$FINANCE_HOME/secrets/<uid>.json.enc` (one master key).
//!
//! **How the tenant travels.** [`Tenant::enter`] makes a tenant current on
//! this thread for the length of a closure; [`crate::config`]'s tree paths
//! (home, data_dir, db_path, transactions_dir), the secrets lookup, the
//! formats dir and everything built on them (db::open, the views, ingest,
//! categorize, the fetch store, export, the archive) then resolve to that
//! tenant. Code that starts a thread carries the tenant over with
//! [`spawn`] / [`carry`].
//!
//! **No tenant set.** A CLI command or a test runs as the owner, exactly as
//! before. A server turns on [`set_strict`]: from then on, resolving a tree
//! path on a thread with no tenant is a bug and panics (the request answers
//! 500, nothing is read) rather than silently falling back to the owner's
//! data. Server-wide files (server.db, the web app, the secrets dir) use
//! [`crate::config::root_home`] / [`crate::config::root_data_dir`], which
//! never depend on a tenant.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config;

/// The owner (the first account, the legacy tree).
pub const OWNER_UID: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tenant {
    pub uid: i64,
    /// the tree's root ($FINANCE_HOME for the owner)
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub db: PathBuf,
    pub transactions: PathBuf,
    pub formats: PathBuf,
    pub avatar: PathBuf,
    /// the owner's tree: the env overrides, `.env` and the process
    /// environment are its secret providers too
    pub legacy: bool,
}

fn var(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

impl Tenant {
    /// The owner's tree as configured now (FINANCE_HOME and the overrides;
    /// re-read every call, as config always did).
    pub fn owner() -> Tenant {
        let home = config::root_home();
        let data_dir = config::root_data_dir();
        Tenant {
            uid: OWNER_UID,
            db: var("FINANCE_DB").unwrap_or_else(|| data_dir.join("finance.db")),
            transactions: var("FINANCE_TRANSACTIONS_DIR").unwrap_or_else(|| home.join("transactions")),
            formats: var("FINANCE_FORMATS_DIR").unwrap_or_else(|| home.join("formats")),
            avatar: data_dir.join("avatar"),
            data_dir,
            home,
            legacy: true,
        }
    }

    /// Where user `uid`'s tree goes: `$FINANCE_HOME/users/<uid>`.
    pub fn default_home(uid: i64) -> PathBuf {
        config::root_home().join("users").join(uid.to_string())
    }

    /// A non-owner user's tree at `home` (the users/<uid>/ layout).
    pub fn user_at(uid: i64, home: &Path) -> Tenant {
        let data_dir = home.join("data");
        Tenant {
            uid,
            db: data_dir.join("finance.db"),
            transactions: home.join("transactions"),
            formats: home.join("formats"),
            avatar: home.join("avatar"),
            data_dir,
            home: home.to_path_buf(),
            legacy: false,
        }
    }

    /// The tenant of server.db's user row (id, home). None when the user
    /// has no tree (an empty home). User 1 is the owner: the legacy tree,
    /// wherever $FINANCE_HOME is now. Any other user's tree must be their
    /// users/<uid> directory of this server: a row pointing anywhere else
    /// (the owner's tree, another user's) is refused, so not even a
    /// tampered row can point one user at another's data.
    pub fn for_user(uid: i64, home: &str) -> Option<Tenant> {
        let home = home.trim();
        if home.is_empty() {
            return None;
        }
        if uid == OWNER_UID {
            return Some(Tenant::owner());
        }
        let h = PathBuf::from(home);
        if canon(&h) != canon(&Self::default_home(uid)) || canon(&h) == canon(&config::root_home()) {
            return None;
        }
        Some(Tenant::user_at(uid, &h))
    }

    /// Run `f` with this tenant current on this thread (restored after,
    /// even on a panic).
    pub fn enter<T>(&self, f: impl FnOnce() -> T) -> T {
        let prev = CURRENT.with(|c| c.replace(Some(Arc::new(self.clone()))));
        struct Restore(Option<Arc<Tenant>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                let p = self.0.take();
                CURRENT.with(|c| *c.borrow_mut() = p);
            }
        }
        let _restore = Restore(prev);
        f()
    }

    /// A short label for logs: "user 2".
    pub fn label(&self) -> String {
        format!("user {}", self.uid)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Tenant>>> = const { RefCell::new(None) };
}

static STRICT: AtomicBool = AtomicBool::new(false);

/// From now on a tree path resolved with no tenant current panics (the
/// server sets this once it serves more than one tree).
pub fn set_strict(on: bool) {
    STRICT.store(on, Ordering::SeqCst);
}

pub fn strict() -> bool {
    STRICT.load(Ordering::SeqCst)
}

/// The tenant current on this thread, if any.
pub fn current() -> Option<Arc<Tenant>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// The tenant to resolve paths for: the current one, else the owner
/// (unless strict, where that is a bug: see the module doc).
pub fn active() -> Arc<Tenant> {
    if let Some(t) = current() {
        return t;
    }
    if strict() {
        panic!("no tenant on this thread: a tree path was resolved outside Tenant::enter");
    }
    Arc::new(Tenant::owner())
}

/// The active tenant's user id.
pub fn uid() -> i64 {
    active().uid
}

/// Wrap `f` so it runs under the tenant current here (or none) wherever it
/// is called: for a closure handed to another thread.
pub fn carry<T>(f: impl FnOnce() -> T) -> impl FnOnce() -> T {
    let t = current();
    move || match t {
        Some(t) => t.enter(f),
        None => f(),
    }
}

/// std::thread::Builder::spawn with the current tenant carried over.
pub fn spawn<T: Send + 'static>(name: &str, f: impl FnOnce() -> T + Send + 'static) -> std::io::Result<std::thread::JoinHandle<T>> {
    std::thread::Builder::new().name(name.into()).spawn(carry(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_scopes_and_restores() {
        let a = Tenant::user_at(2, Path::new("/x/users/2"));
        let b = Tenant::user_at(3, Path::new("/x/users/3"));
        assert!(current().is_none());
        a.enter(|| {
            assert_eq!(uid(), 2);
            b.enter(|| assert_eq!(uid(), 3));
            assert_eq!(uid(), 2);
            let r = std::panic::catch_unwind(|| b.enter(|| panic!("x")));
            assert!(r.is_err());
            assert_eq!(uid(), 2, "restored after a panic");
            let h = spawn("t", || uid()).unwrap();
            assert_eq!(h.join().unwrap(), 2, "carried to the thread");
        });
        assert!(current().is_none());
    }

    #[test]
    fn user_layout() {
        let t = Tenant::user_at(7, Path::new("/srv/f/users/7"));
        assert_eq!(t.db, PathBuf::from("/srv/f/users/7/data/finance.db"));
        assert_eq!(t.transactions, PathBuf::from("/srv/f/users/7/transactions"));
        assert_eq!(t.formats, PathBuf::from("/srv/f/users/7/formats"));
        assert_eq!(t.avatar, PathBuf::from("/srv/f/users/7/avatar"));
        assert!(!t.legacy);
        assert!(Tenant::for_user(7, "").is_none());
        assert!(Tenant::for_user(7, "/etc").is_none(), "a home outside users/<uid> is refused");
    }
}
