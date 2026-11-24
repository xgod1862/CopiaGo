//! Observe paired-phone connections without setting either clipboard.

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    match windows::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("connection_probe: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("connection_probe requires Windows");
    ExitCode::FAILURE
}

#[cfg(windows)]
mod windows {
    use std::{
        cell::Cell,
        env,
        ops::ControlFlow,
        time::{Duration, Instant},
    };

    use copiago_core::paths::{RunMode, data_dir};
    use copiago_device::{
        ConnState, DeviceCommand, DeviceEvent, DeviceRunOptions, HostSignal, run_device,
    };
    use tokio::sync::mpsc;

    const USAGE: &str =
        "usage: connection_probe [--seconds N] [--simulate-resume] [--write-device-data]";

    pub(super) fn run() -> Result<(), &'static str> {
        let mut seconds = 30;
        let mut write = false;
        let mut resume = false;
        let mut args = env::args_os().skip(1);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--seconds") => {
                    seconds = args
                        .next()
                        .and_then(|value| value.to_str()?.parse::<u32>().ok())
                        .filter(|&seconds| seconds > 0)
                        .ok_or(USAGE)?;
                }
                Some("--write-device-data") => write = true,
                Some("--simulate-resume") => resume = true,
                _ => return Err(USAGE),
            }
        }
        if env::var_os("ICB_DATA_DIR").is_none_or(|path| path.is_empty()) {
            return Err("set ICB_DATA_DIR to a protected copy of the CopiaGo data directory");
        }
        let root =
            data_dir(RunMode::Development).map_err(|_| "could not resolve data directory")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "could not start runtime")?;

        let options = DeviceRunOptions {
            persist_device_data: write,
            ..DeviceRunOptions::default()
        };
        let started = Instant::now();
        let connected = Cell::new(0);
        let resume_at = Cell::new(None::<Instant>);
        let (send, mut commands) = mpsc::channel(1);
        let (signal, signals) = mpsc::channel(1);
        let worker = run_device(&root, options, &mut commands, signals, |event| {
            let elapsed = started.elapsed().as_millis();
            match event {
                DeviceEvent::Status(ConnState::Connected { wireless }, _) => {
                    println!("elapsed_ms={elapsed} connected wireless={wireless}");
                    connected.set(connected.get() + 1);
                    if let Some(requested) = resume_at.take() {
                        println!(
                            "simulated_resume_recovery_ms={}",
                            requested.elapsed().as_millis()
                        );
                    }
                    if resume && connected.get() == 1 {
                        resume_at.set(Some(Instant::now()));
                        if signal.try_send(HostSignal::Resumed).is_ok() {
                            println!("elapsed_ms={elapsed} simulated_resume");
                        }
                    }
                }
                DeviceEvent::Status(state, text) => {
                    println!(
                        "elapsed_ms={elapsed} state={state:?} {}",
                        text.render(options.language)
                    );
                }
                DeviceEvent::DeveloperServicesMissing { wireless } => {
                    println!("elapsed_ms={elapsed} developer_services_missing wireless={wireless}");
                }
                DeviceEvent::DeviceInfo(info) => {
                    println!(
                        "elapsed_ms={elapsed} device_info name_present={} model_present={} os_version_present={}",
                        !info.name.is_empty(),
                        !info.model.is_empty(),
                        !info.os_version.is_empty(),
                    );
                }
                DeviceEvent::Clip {
                    while_disconnected, ..
                } => {
                    println!(
                        "elapsed_ms={elapsed} clip_received while_disconnected={while_disconnected}"
                    );
                }
                DeviceEvent::FileOffer(_) => println!("elapsed_ms={elapsed} file_offered"),
                _ => {}
            }
            ControlFlow::Continue(())
        });
        runtime.block_on(async {
            tokio::join!(worker, async {
                tokio::time::sleep(Duration::from_secs(u64::from(seconds))).await;
                let _ = send.send(DeviceCommand::Shutdown).await;
            });
        });
        if connected.get() == 0 {
            return Err("no connection was ready during the probe");
        }
        if resume && connected.get() < 2 {
            return Err("connection was not restored after simulated resume");
        }
        Ok(())
    }
}
