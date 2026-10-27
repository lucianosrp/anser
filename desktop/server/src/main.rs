//! anser-server: the finance server and its CLI.
//! Each subcommand lives in its own cmd/ module (one owner per file).

mod cmd;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let sub = args.next().unwrap_or_else(|| "help".into());
    let rest: Vec<String> = args.collect();
    cmd::dispatch(&sub, &rest)
}
