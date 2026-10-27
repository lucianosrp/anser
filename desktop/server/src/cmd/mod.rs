//! Subcommand table: ingest, categorize, txns, override, fetch-crypto,
//! fetch-fx, refresh-prices, report, stats, backup, serve, mcp (+ mcp-key,
//! mcp-enable, mcp-disable: the HTTP endpoint's keys and switch), sync (all
//! fetchers), push (send local files/overrides to the remote),
//! install-webapp (send a built web app bundle to a server); pair, user,
//! auth, secrets (account.rs: devices, the auth mode, the secrets store);
//! export, import, verify (archives); --version.

mod account;
mod archive;
mod args;
mod backup;
mod categorize;
mod fetch_crypto;
mod fetch_fx;
mod ingest;
mod install_webapp;
mod mcp;
mod override_;
mod push;
mod refresh_prices;
mod report;
mod serve;
mod stats;
mod sync;
mod txns;

/// The data commands: they run on one user's tree, the owner's unless
/// `--user NAME|ID` names another (finance_engine::tenant).
const PER_USER: [&str; 18] = [
    "ingest", "categorize", "txns", "override", "stats", "backup", "mcp", "mcp-key", "mcp-enable", "mcp-disable", "sync", "fetch-crypto", "fetch-fx", "refresh-prices", "report",
    "export", "import", "pair",
];

/// `--user V` / `--user=V` taken out of `args`.
fn take_user(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut user = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--user" {
            user = it.next().cloned();
        } else if let Some(v) = a.strip_prefix("--user=") {
            user = Some(v.to_string());
        } else {
            rest.push(a.clone());
        }
    }
    (user, rest)
}

pub fn dispatch(sub: &str, args: &[String]) -> anyhow::Result<()> {
    if PER_USER.contains(&sub) {
        let (user, rest) = take_user(args);
        if let Some(u) = user {
            let t = finance_server::auth::tenant_of(&u)?;
            return t.enter(|| run(sub, &rest));
        }
    }
    run(sub, args)
}

fn run(sub: &str, args: &[String]) -> anyhow::Result<()> {
    match sub {
        "ingest" => ingest::run(args),
        "categorize" => categorize::run(args),
        "txns" => txns::run(args),
        "override" => override_::run(args),
        "stats" => stats::run(args),
        "backup" => backup::run(args),
        "serve" => serve::run(args),
        "mcp" => mcp::run(args),
        "mcp-key" => mcp::key(args),
        "mcp-enable" => mcp::enable(args),
        "mcp-disable" => mcp::disable(args),
        "sync" => sync::run(args),
        "fetch-crypto" => fetch_crypto::run(args),
        "fetch-fx" => fetch_fx::run(args),
        "refresh-prices" => refresh_prices::run(args),
        "report" => report::run(args),
        "push" => push::run(args),
        "install-webapp" => install_webapp::run(args),
        "--version" | "-V" | "version" => {
            println!("anser-server {}", finance_core::VERSION);
            Ok(())
        }
        "export" => archive::export(args),
        "import" => archive::import(args),
        "verify" => archive::verify(args),
        "pair" => account::pair(args),
        "user" => account::user(args),
        "auth" => account::auth_mode(args),
        "secrets" => account::secrets_cmd(args),
        _ => {
            eprintln!("usage: anser-server <command> [args]  (unknown: {sub})");
            std::process::exit(2)
        }
    }
}
