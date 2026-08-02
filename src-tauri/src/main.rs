// Prevents an additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `agentsbar status [--json]`, `--version` and `--help` run in the invoking terminal
    // and exit before the GUI starts. With no subcommand the tray app launches as usual.
    if agentsbar_lib::cli::wants_cli() {
        agentsbar_lib::cli::main();
        return;
    }
    agentsbar_lib::run();
}
