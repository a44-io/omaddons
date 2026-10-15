use std::process::ExitCode;

fn main() -> ExitCode {
    omaddons::cli::run(std::env::args_os())
}
