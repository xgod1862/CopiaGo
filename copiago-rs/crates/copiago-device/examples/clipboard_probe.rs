//! Report the first new text event from an already-trusted USB iPhone.

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    use std::ops::ControlFlow;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use copiago_core::{
        clip::Clip,
        i18n::LanguagePreference,
        paths::{RunMode, data_dir},
    };
    use copiago_device::{ConnState, DeviceEvent, watch_usb};

    let mut args = std::env::args_os().skip(1);
    let root = match args.next().map_or_else(
        || data_dir(RunMode::Development),
        |root| Ok(PathBuf::from(root)),
    ) {
        Ok(root) => root,
        Err(_) => {
            eprintln!("clipboard_probe: could not resolve data directory");
            return ExitCode::FAILURE;
        }
    };
    let seconds = args.next().map_or(Some(60), |value| {
        value.to_str()?.parse::<u64>().ok().filter(|&n| n > 0)
    });
    let (Some(seconds), None) = (seconds, args.next()) else {
        eprintln!("usage: clipboard_probe [copiago-data-root] [wait-seconds]");
        return ExitCode::FAILURE;
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("clipboard_probe: could not start runtime");
            return ExitCode::FAILURE;
        }
    };

    let started = Instant::now();
    let mut ready = None;
    let watch = watch_usb(&root, LanguagePreference::System.resolve(), |event| {
        match event {
            DeviceEvent::Status(ConnState::Connected { .. }, _) => {
                ready = Some(Instant::now());
                println!("ready elapsed_ms={}", started.elapsed().as_millis());
            }
            DeviceEvent::Clip {
                clip: Clip::Text(_),
                ..
            } => {
                let elapsed = ready.unwrap_or(started).elapsed();
                println!("text_event elapsed_since_ready_ms={}", elapsed.as_millis());
                return ControlFlow::Break(());
            }
            _ => {}
        }
        ControlFlow::Continue(())
    });

    match runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(seconds), watch).await })
    {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(error)) => {
            eprintln!(
                "clipboard_probe: {error} (elapsed_ms={})",
                started.elapsed().as_millis()
            );
            ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!("clipboard_probe: no new text event within {seconds} s");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("clipboard_probe requires Windows");
    ExitCode::FAILURE
}
