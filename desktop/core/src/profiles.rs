//! The accounts this machine signs in with: one profile per (server, user).
//!
//! A household server has several users, and a laptop may talk to more
//! than one server, so the app keeps a list of profiles in `profiles.json`
//! next to the server file (`~/.config/finance-desktop/`):
//!
//! ```json
//! {"active": "server.tail1234.ts.net",
//!  "profiles": [{"id": "server.tail1234.ts.net", "url": "https://server.tail1234.ts.net",
//!                "label": "Alex", "user_id": 1, "token_key": "https://server.tail1234.ts.net"}]}
//! ```
//!
//! * `id` is also the profile's cache segment: its replica (finance.db,
//!   export.json, the avatar, replica.json; encrypted or not, with its own
//!   key) lives in `<data dir>/profiles/<id>/`, so two accounts never share
//!   a local copy.
//! * `token_key` names its device token (auth.rs: the keyring entry or the
//!   tokens.json key). The first profile of a server keeps the plain URL,
//!   the key it always had, so an existing pairing carries over; another
//!   account on the same server gets `URL#id`.
//! * The server file keeps the active profile's URL on its first line, as
//!   before, for anything that only reads that.
//!
//! First run of this version: the single replica in the data dir moves into
//! the first profile's segment (renamed in place, no download; an encrypted
//! copy's keyring key moves with it), and the server file's URL becomes that
//! profile.
//!
//! **Unfinished accounts** ([`is_unfinished`]). *Add an account…* makes a
//! profile and restarts on the wizard; until it signs in it has nothing. A
//! profile is unfinished when it is not the demo, has no device token under
//! its `token_key` (`$FINANCE_TOKEN` counts as one), and has no local copy
//! (no `finance.db` or `export.json`, plain or `.enc`, in its folder). The
//! name doesn't matter: an account signed out elsewhere (its token gone,
//! no copy left) is as unusable as one that never signed in. One with a
//! copy stays whatever else it lacks: the owner can still look at it and
//! sign in again. They go:
//!
//! * at start ([`prune_unfinished`], before the window): every unfinished
//!   profile, except the one *Add an account…* restarted on (that start
//!   only) and never the last profile on the machine (a first run's single
//!   profile is unfinished until it signs in). When the active one goes,
//!   the one in use before it (`previous`) is active again, else the first.
//! * when the wizard closes before the added account signed in
//!   ([`cancel_unfinished`]): that profile goes, back to `previous`.
//!
//! Going ([`remove`] too) takes the profile's token key (unless another
//! profile shares it), its folder under `profiles/` and its copy's keyring
//! key. Backup keys stay: an id can be reused after a sign-out, and the
//! key under it may be an older account's, the only way to its archives.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cache;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub url: String,
    /// who it is: the user's name once known (whoami), else empty
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub user_id: Option<i64>,
    pub token_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profiles {
    pub active: String,
    pub profiles: Vec<Profile>,
    /// the one active before `active` was picked: where an unfinished
    /// account goes back to
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub previous: String,
}

impl Profiles {
    pub fn active(&self) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == self.active).or(self.profiles.first())
    }
}

pub fn norm(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

pub fn file() -> PathBuf {
    cache::server_file().with_file_name("profiles.json")
}

/// The data dir the profiles' segments live under: `$FINANCE_DESKTOP_DATA`,
/// else `~/.local/share/finance-desktop`.
pub fn base_dir() -> PathBuf {
    std::env::var_os("FINANCE_DESKTOP_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_else(|| ".".into()).join("finance-desktop"))
}

pub fn dir_of(id: &str) -> PathBuf {
    base_dir().join("profiles").join(id)
}

/// `https://server.tail1234.ts.net:8787/` -> `server.tail1234.ts.net-8787`.
pub fn slug(s: &str) -> String {
    let s = s.split_once("://").map_or(s, |x| x.1);
    let out: String = s
        .trim_end_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let out = out.trim_matches(['-', '.']).to_string();
    if out.is_empty() { "server".into() } else { out.chars().take(60).collect() }
}

/// (the file as read, the process's copy): read once, then kept in step
/// with every write here.
static CACHE: Mutex<Option<(PathBuf, Profiles)>> = Mutex::new(None);

fn read_file(p: &Path) -> Option<Profiles> {
    std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<Profiles>(&b).ok()).filter(|ps| !ps.profiles.is_empty())
}

fn write_file(ps: &Profiles) -> Result<()> {
    let p = file();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(ps)?).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("writing {}", p.display()))?;
    // the server file keeps the active URL on its first line, as before
    // (never the demo's: it isn't a server)
    if let Some(a) = ps.active().filter(|a| !is_demo(a)) {
        let _ = cache::write_server_file(&a.url);
    }
    Ok(())
}

/// The profiles, made on first use from the server file (and the single
/// replica moved into the first one's segment).
pub fn load() -> Profiles {
    let p = file();
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((path, ps)) = g.as_ref() {
        if *path == p {
            return ps.clone();
        }
    }
    let ps = read_file(&p).unwrap_or_else(|| {
        let first = default_profile();
        let ps = Profiles { active: first.id.clone(), profiles: vec![first.clone()], ..Default::default() };
        if let Err(e) = migrate_replica(&first.id) {
            eprintln!("profiles: the local copy stayed where it was ({e:#}); the next sync fetches it again");
        }
        let _ = write_file(&ps);
        ps
    });
    *g = Some((p, ps.clone()));
    ps
}

fn store(ps: Profiles) -> Result<()> {
    write_file(&ps)?;
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((file(), ps));
    Ok(())
}

pub fn list() -> Vec<Profile> {
    load().profiles
}

/// The profile in use.
pub fn active() -> Profile {
    let ps = load();
    ps.active().cloned().expect("load() always has one")
}

pub fn set_active(id: &str) -> Result<()> {
    let mut ps = load();
    if !ps.profiles.iter().any(|p| p.id == id) {
        bail!("no profile {id}");
    }
    if ps.active != id {
        ps.previous = std::mem::replace(&mut ps.active, id.to_string());
    }
    store(ps)
}

/// A new profile for `url` (not active yet). Its token key is the URL when
/// no other profile holds that one, else `URL#id`.
pub fn add(url: &str, label: &str) -> Result<Profile> {
    let url = norm(url);
    anyhow::ensure!(url.starts_with("http://") || url.starts_with("https://"), "a server URL starts with http:// or https://");
    let mut ps = load();
    let base = if label.trim().is_empty() { format!("{}-account", slug(&url)) } else { format!("{}-{}", slug(&url), slug(label)) };
    let mut id = base.clone();
    let mut n = 2;
    while ps.profiles.iter().any(|p| p.id == id) || dir_of(&id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let taken = ps.profiles.iter().any(|p| p.token_key == url);
    let token_key = if taken { format!("{url}#{id}") } else { url.clone() };
    let p = Profile { id, url, label: label.trim().to_string(), user_id: None, token_key };
    ps.profiles.push(p.clone());
    store(ps)?;
    Ok(p)
}

/// Remove a profile: its token, its local copy (a cache: the server keeps
/// the data). The last profile can't go; removing the active one makes the
/// first other one active.
pub fn remove(id: &str) -> Result<()> {
    let ps = load();
    if !ps.profiles.iter().any(|p| p.id == id) {
        bail!("no profile {id}");
    }
    if ps.profiles.len() == 1 {
        bail!("the only account on this computer: sign out instead");
    }
    drop_profiles(ps, &[id.to_string()], false)
}

/// Take `ids` out of `ps` and off this computer: each one's token key
/// (unless a remaining profile shares it), folder and copy's keyring key.
/// When the active one goes, `previous` (with `back`, else the first one
/// left) is active again.
fn drop_profiles(mut ps: Profiles, ids: &[String], back: bool) -> Result<()> {
    let (gone, keep): (Vec<Profile>, Vec<Profile>) = std::mem::take(&mut ps.profiles).into_iter().partition(|p| ids.contains(&p.id));
    ps.profiles = keep;
    anyhow::ensure!(!ps.profiles.is_empty(), "the only account on this computer: sign out instead");
    for p in &gone {
        if !ps.profiles.iter().any(|o| o.token_key == p.token_key) {
            let _ = crate::auth::forget_key(&p.token_key);
        }
        let d = dir_of(&p.id);
        if d.starts_with(base_dir().join("profiles")) {
            crate::replica::forget_key_of(&d);
            let _ = std::fs::remove_dir_all(&d);
        }
    }
    let here = |ps: &Profiles, id: &str| ps.profiles.iter().any(|p| p.id == id);
    if !here(&ps, &ps.active) {
        ps.active = if back && here(&ps, &ps.previous) { ps.previous.clone() } else { ps.profiles[0].id.clone() };
    }
    if !here(&ps, &ps.previous) || ps.previous == ps.active {
        ps.previous.clear();
    }
    store(ps)
}

/// It has a local copy to show: the replica or the bundle, plain or
/// encrypted.
pub fn has_copy(id: &str) -> bool {
    let d = dir_of(id);
    [cache::DB_FILE, cache::BUNDLE_FILE].iter().any(|f| d.join(f).exists() || d.join(format!("{f}.enc")).exists())
}

/// Nothing to sign in with and nothing to show (see the module doc).
/// Asks the keyring only when there is no copy.
pub fn is_unfinished(p: &Profile) -> bool {
    !is_demo(p) && !has_copy(&p.id) && !crate::auth::has_token_key(&p.token_key)
}

/// At start, before the window: the unfinished profiles go (see the module
/// doc), all but `adding` (the one *Add an account…* restarted on) and
/// never the last one (the active one stays then). Returns their ids.
pub fn prune_unfinished(adding: Option<&str>) -> Vec<String> {
    let ps = load();
    let mut ids: Vec<String> = ps.profiles.iter().filter(|p| Some(p.id.as_str()) != adding && is_unfinished(p)).map(|p| p.id.clone()).collect();
    if ids.len() == ps.profiles.len() {
        let active = ps.active().map(|a| a.id.clone()).unwrap_or_default();
        ids.retain(|id| *id != active);
    }
    if ids.is_empty() {
        return ids;
    }
    match drop_profiles(ps, &ids, true) {
        Ok(()) => ids,
        Err(e) => {
            eprintln!("profiles: the unfinished accounts stayed ({e:#})");
            vec![]
        }
    }
}

/// The wizard on an account that hasn't signed in can be left: there is
/// another account to go back to.
pub fn cancellable() -> bool {
    let ps = load();
    ps.profiles.len() > 1 && ps.active().is_some_and(is_unfinished)
}

/// The wizard closed before the active account signed in (*Add an
/// account…*, then the X): it goes, and the account in use before is back.
/// Ok(true) when it went (the caller restarts on that one).
pub fn cancel_unfinished() -> Result<bool> {
    if !cancellable() {
        return Ok(false);
    }
    let ps = load();
    let id = ps.active().map(|a| a.id.clone()).unwrap_or_default();
    drop_profiles(ps, &[id], true)?;
    Ok(true)
}

/// Delete all, on this computer: every account's device token (and its
/// server's resume key), local copy and its key, then the app's settings, so the next start is a first run.
/// The backup keys stay when `keep_backup_key` (they alone open the
/// server's nightly archives). The server is untouched.
pub fn wipe_local(keep_backup_key: bool) -> Result<()> {
    let ps = load();
    for p in &ps.profiles {
        // the keyring entries are named after the active account: each in turn
        store(Profiles { active: p.id.clone(), profiles: ps.profiles.clone(), ..Default::default() })?;
        let _ = crate::replica::forget(&[]);
        if !keep_backup_key {
            crate::backups::forget_identity();
        }
        let _ = crate::auth::forget_key(&p.token_key);
        let _ = crate::auth::forget_resume_key(&p.url);
    }
    let base = base_dir();
    if base.ends_with("finance-desktop") || std::env::var_os("FINANCE_DESKTOP_DATA").is_some() {
        match std::fs::remove_dir_all(&base) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("removing {}", base.display())),
            _ => {}
        }
    }
    // the settings next to the server file: only the app's own files
    let conf = cache::server_file();
    let conf = conf.parent().unwrap_or(Path::new("."));
    for e in std::fs::read_dir(conf).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let backup_key = name.starts_with("backup-identity");
        let ours = matches!(name.as_str(), "server" | "profiles.json" | "tokens.json" | "theme.toml" | "formats")
            || name.ends_with(".toml") || name.ends_with(".toml.enc") || backup_key;
        if ours && !(backup_key && keep_backup_key) {
            let p = e.path();
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }
    Ok(())
}

/// What whoami said about the active profile's account (the menu shows it).
/// Signing in again as an account this computer already had (Add an
/// account, after a sign-out) leaves two profiles for one user: the older
/// one goes, with its token and its local copy.
pub fn set_identity(label: &str, user_id: Option<i64>) -> Result<()> {
    let mut ps = load();
    let active = ps.active.clone();
    let Some(p) = ps.profiles.iter_mut().find(|p| p.id == active) else { return Ok(()) };
    let fresh = p.label != label || p.user_id != user_id;
    p.label = label.to_string();
    p.user_id = user_id;
    let dupes = ps.drop_duplicates();
    if !fresh && dupes.is_empty() {
        return Ok(());
    }
    for d in &dupes {
        let _ = crate::auth::forget_key(&d.token_key);
        let dir = dir_of(&d.id);
        if dir.starts_with(base_dir().join("profiles")) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    store(ps)
}

impl Profiles {
    /// Take out the other profiles of the active one's server and user.
    fn drop_duplicates(&mut self) -> Vec<Profile> {
        let Some(a) = self.profiles.iter().find(|p| p.id == self.active).cloned() else { return vec![] };
        if a.user_id.is_none() {
            return vec![];
        }
        let (dupes, keep) = std::mem::take(&mut self.profiles).into_iter()
            .partition(|p: &Profile| p.id != a.id && p.url == a.url && p.user_id == a.user_id);
        self.profiles = keep;
        dupes
    }
}

/// The active profile's server moved (Settings, the Doctor, the wizard).
pub fn set_active_url(url: &str) -> Result<()> {
    let mut ps = load();
    if ps.active().is_some_and(is_demo) {
        bail!("{}", crate::demo::NOTE);
    }
    let active = ps.active.clone();
    let url = norm(url);
    if let Some(p) = ps.profiles.iter_mut().find(|p| p.id == active) {
        if p.token_key == p.url {
            p.token_key = url.clone();
        } else if let Some((_, id)) = p.token_key.split_once('#') {
            p.token_key = format!("{url}#{id}");
        }
        p.url = url;
    }
    store(ps)
}

/// The token key for `server`: the active profile's when it is that
/// server, else the URL itself.
pub fn token_key(server: &str) -> String {
    let s = norm(server);
    if !file().exists() {
        // no profiles yet (a tool running before the app): the URL, as always
        return s;
    }
    let a = active();
    if a.url == s { a.token_key } else { s }
}

/// The active profile is an account added on this machine (not the first
/// one of its server): it talks to the server only with its own token.
pub fn needs_token() -> bool {
    file().exists() && {
        let a = active();
        a.token_key != a.url
    }
}

/// The calls an account makes before it has a token: who am I, pairing,
/// signing up or in.
pub fn anonymous_ok(path: &str) -> bool {
    matches!(path, "/api/whoami" | "/api/pair" | "/api/signup" | "/api/login" | "/healthz" | "/api/version")
}

/// A server's short name: "https://myserver.tail1234.ts.net:8787" ->
/// "myserver" (an IP stays whole, without the port).
pub use crate::client::short_host;

/// What a label shows: the user's name and the server's short name; with
/// no name yet, "Not signed in · myserver".
pub fn display(p: &Profile) -> String {
    if is_demo(p) {
        return crate::demo::LABEL.into();
    }
    let host = short_host(&p.url);
    let host = if host.is_empty() { p.url.as_str() } else { host };
    if p.label.is_empty() { format!("Not signed in · {host}") } else { format!("{} · {host}", p.label) }
}

/// The accounts the avatar menu lists: the active one, and every other one
/// but an unnamed one with no local copy (an unfinished account the start
/// didn't prune; no keyring asked, it runs on every render).
pub fn menu_list() -> Vec<Profile> {
    let ps = load();
    let active = ps.active().map(|a| a.id.clone()).unwrap_or_default();
    ps.profiles.into_iter().filter(|p| p.id == active || is_demo(p) || !p.label.is_empty() || p.user_id.is_some() || has_copy(&p.id)).collect()
}

/// The demo account's profile (crate::demo)?
pub fn is_demo(p: &Profile) -> bool {
    crate::demo::is_url(&p.url)
}

/// The profile a machine starts with: the server file's URL (never the
/// demo's), else a server on this machine.
fn default_profile() -> Profile {
    let url = cache::saved_server_file_url().filter(|u| !crate::demo::is_url(u)).unwrap_or_else(|| cache::DEFAULT_SERVER.into());
    Profile { id: slug(&url), url: url.clone(), label: String::new(), user_id: None, token_key: norm(&url) }
}

/// "Just try it": the demo profile, made the active one. Its replica
/// (`dir_of(demo::ID)`) is the caller's to build first; nothing else
/// changes, so leaving it puts the machine back as it was.
pub fn enter_demo() -> Result<()> {
    let mut ps = load();
    ps.profiles.retain(|p| !is_demo(p));
    let id = crate::demo::ID.to_string();
    ps.profiles.push(Profile { id: id.clone(), url: crate::demo::URL.into(), label: crate::demo::NAME.into(), user_id: None,
                               token_key: crate::demo::URL.into() });
    ps.active = id;
    store(ps)
}

/// Leave the demo: its profile and its replica go; the account in use
/// before it (else a fresh one for the server file's URL) is active again.
/// Returns whether that account has a local copy to show.
pub fn leave_demo() -> Result<bool> {
    let mut ps = load();
    let was_active = ps.active().is_some_and(is_demo);
    ps.profiles.retain(|p| !is_demo(p));
    let d = dir_of(crate::demo::ID);
    if d.starts_with(base_dir().join("profiles")) {
        let _ = std::fs::remove_dir_all(&d);
    }
    if ps.profiles.is_empty() {
        ps.profiles.push(default_profile());
    }
    if was_active || !ps.profiles.iter().any(|p| p.id == ps.active) {
        ps.active = ps.profiles[0].id.clone();
    }
    let has_copy = ps.active().is_some_and(|a| dir_of(&a.id).join(cache::DB_FILE).exists() || dir_of(&a.id).join(format!("{}.enc", cache::DB_FILE)).exists());
    store(ps)?;
    Ok(has_copy)
}

/// Move the single replica (everything in the data dir but `profiles/`)
/// into `profiles/<id>/`. Renames only: nothing is downloaded again. The
/// encrypted copy's keyring key is keyed by the folder, so it moves too.
fn migrate_replica(id: &str) -> Result<()> {
    let base = base_dir();
    let dest = base.join("profiles").join(id);
    let Ok(rd) = std::fs::read_dir(&base) else { return Ok(()) };
    let entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n != "profiles")).collect();
    if entries.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;
    crate::replica::move_key(&base, &dest);
    for e in entries {
        let to = dest.join(e.file_name().unwrap_or_default());
        std::fs::rename(&e, &to).with_context(|| format!("moving {} to {}", e.display(), to.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("https://server.tail1234.ts.net/"), "server.tail1234.ts.net");
        assert_eq!(slug("http://127.0.0.1:8787"), "127.0.0.1-8787");
        assert_eq!(slug("Ann Smith"), "ann-smith");
        assert_eq!(slug("///"), "server");
    }

    #[test]
    fn signing_in_again_drops_the_older_profile() {
        let p = |id: &str, url: &str, uid: Option<i64>| Profile { id: id.into(), url: url.into(), label: "A".into(), user_id: uid, token_key: id.into() };
        let mut ps = Profiles { active: "srv-account".into(), profiles: vec![
            p("srv", "https://srv", Some(1)), p("srv-account", "https://srv", Some(1)),
            p("srv-2", "https://srv", Some(2)), p("other", "https://other", Some(1)), p("new", "https://srv", None)], ..Default::default() };
        let gone: Vec<String> = ps.drop_duplicates().into_iter().map(|p| p.id).collect();
        assert_eq!(gone, ["srv"]);
        assert_eq!(ps.profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["srv-account", "srv-2", "other", "new"]);
        // an account not identified yet never takes another out
        ps.active = "new".into();
        assert!(ps.drop_duplicates().is_empty());
    }
}
