//! anser-server: the finance server and its CLI, the headless build (no GUI
//! libraries). The desktop app runs the same CLI as `anser server`.
//! Each subcommand lives in its own cli/ module (one owner per file).

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    finance_server::cli::run_main("anser-server", &args)
}
