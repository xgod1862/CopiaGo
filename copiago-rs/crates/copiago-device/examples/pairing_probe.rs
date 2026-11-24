//! Exercise the device worker's pairing and forget commands against an isolated data root.

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    match windows::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("pairing_probe: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("pairing_probe requires Windows");
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

    use copiago_core::{
        DeviceInfo,
        i18n::Language,
        paths::{RunMode, data_dir},
    };
    use copiago_device::{ConnState, DeviceCommand, DeviceEvent, DeviceRunOptions, run_device};
    use tokio::sync::mpsc;

    pub(super) fn run() -> Result<(), &'static str> {
        let args: Vec<_> = env::args_os().skip(1).collect();
        let pair = match args.as_slice() {
            [action] if action == "pair" => true,
            [action] if action == "forget" => false,
            _ => return Err("usage: pairing_probe pair|forget"),
        };
        if env::var_os("ICB_DATA_DIR").is_none_or(|path| path.is_empty()) {
            return Err("set ICB_DATA_DIR to a separate temporary data directory");
        }
        let root =
            data_dir(RunMode::Development).map_err(|_| "could not resolve data directory")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "could not start runtime")?;
        let options = DeviceRunOptions {
            language: Language::ZhCn,
            ..DeviceRunOptions::default()
        };
        let (send, mut commands) = mpsc::channel(1);
        let command = if pair {
            DeviceCommand::RequestPair
        } else {
            DeviceCommand::Forget
        };
        send.try_send(command)
            .map_err(|_| "could not queue command")?;

        let started = Instant::now();
        let pairing = Cell::new(false);
        let cleared = Cell::new(false);
        let result = Cell::new(Err("device worker stopped"));
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(125), run_device(&root, options, &mut commands, (), |event| {
                let elapsed = started.elapsed().as_millis();
                let completion = match event {
                    DeviceEvent::Status(ConnState::Pairing, _) => {
                        pairing.set(true);
                        None
                    }
                    DeviceEvent::Status(state, message) => {
                        println!("elapsed_ms={elapsed} state={state:?} {}", message.render(options.language));
                        match state {
                            ConnState::Setup if pair && pairing.get() => Some(Err("pairing failed")),
                            ConnState::Setup if !pair && cleared.get() => Some(Ok(())),
                            ConnState::Connecting | ConnState::Error if pair => {
                                Some(Err("pair requires a data directory with no existing pairing record"))
                            }
                            _ => None,
                        }
                    }
                    DeviceEvent::PairingPrompt(message) => {
                        println!("elapsed_ms={elapsed} {}", message.render(options.language));
                        None
                    }
                    DeviceEvent::DeviceInfo(_) if pair && pairing.get() => {
                        println!("elapsed_ms={elapsed} pairing_saved=true");
                        Some(Ok(()))
                    }
                    DeviceEvent::DeviceInfo(info) if !pair && info == DeviceInfo::default() => {
                        cleared.set(true);
                        None
                    }
                    DeviceEvent::Error(message) => {
                        eprintln!("elapsed_ms={elapsed} {}", message.render(options.language));
                        Some(Err("device command failed"))
                    }
                    _ => None,
                };
                if let Some(completion) = completion {
                    result.set(completion);
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })).await
        }).map_err(|_| "device command timed out")?;
        result.into_inner()
    }
}
