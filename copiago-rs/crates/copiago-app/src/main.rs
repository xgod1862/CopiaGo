#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{env, process::ExitCode};

#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod gui;
#[cfg(windows)]
mod headless;
#[cfg(windows)]
mod launch;
#[cfg(windows)]
mod window;
#[cfg(windows)]
mod worker;

const USAGE: &str = "Usage: copiago [--minimized | --headless]";

#[cfg(windows)]
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let flag = args.next();
    match (flag.as_deref().and_then(|arg| arg.to_str()), args.next()) {
        (Some("--headless"), None) => run(true, false),
        (Some("--minimized"), None) => run(false, true),
        (Some("--help" | "-h"), None) => {
            attach_console();
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        (None, None) if flag.is_none() => run(false, false),
        _ => {
            attach_console();
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(windows)]
fn run(headless: bool, minimized: bool) -> ExitCode {
    if headless {
        attach_console();
    }
    let start = launch::Launch::acquire(minimized || headless);
    let success = match start {
        Ok(None) => true,
        Ok(Some(launch)) if headless => headless::run(launch),
        Ok(Some(launch)) => gui::run(launch, minimized),
        Err(error) => {
            gui::report_error("instance", &error);
            if headless {
                eprintln!("Could not start CopiaGo: {error}");
            }
            false
        }
    };
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: This attaches the calling process to its parent's console, when available.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(windows))]
fn attach_console() {}

#[cfg(not(windows))]
fn run(_: bool, _: bool) -> ExitCode {
    eprintln!("CopiaGo requires Windows");
    ExitCode::FAILURE
}
