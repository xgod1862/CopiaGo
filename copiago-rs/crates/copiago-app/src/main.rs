use std::{env, process::ExitCode};

#[cfg(windows)]
mod headless;

const USAGE: &str = "Usage: copiago --headless";

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let flag = args.next();
    match (flag.as_deref().and_then(|arg| arg.to_str()), args.next()) {
        (Some("--headless"), None) => run_headless(),
        (Some("--help" | "-h"), None) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        (None, None) if flag.is_none() => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(windows)]
fn run_headless() -> ExitCode {
    if headless::run() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(not(windows))]
fn run_headless() -> ExitCode {
    eprintln!("copiago --headless requires Windows");
    ExitCode::FAILURE
}
