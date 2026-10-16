use std::env;
use std::process::ExitCode;

const HELP: &str = "\
pulpo - run Pulpo collections from the terminal

USAGE:
    pulpo <COMMAND>

COMMANDS:
    version    Print the version
    help       Print this help

More commands (run, fmt, scan) are coming. See https://github.com/pulpo-api/pulpo";

fn main() -> ExitCode {
    let command = env::args().nth(1);
    match command.as_deref() {
        None | Some("help") | Some("-h") | Some("--help") => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("version") | Some("-V") | Some("--version") => {
            println!("pulpo {}", pulpo_core::VERSION);
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command: {other}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}
