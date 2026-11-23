//! Inspect only the service table of an already-trusted USB iPhone.

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    use std::path::PathBuf;
    use std::time::Instant;

    use copiago_core::paths::{RunMode, data_dir};
    use copiago_device::{PairingRecordSource, probe_usb};

    let mut args = std::env::args_os().skip(1);
    let root = match (args.next(), args.next()) {
        (None, None) => match data_dir(RunMode::Development) {
            Ok(root) => root,
            Err(_) => {
                eprintln!("usb_probe: could not resolve data directory");
                return ExitCode::FAILURE;
            }
        },
        (Some(root), None) => PathBuf::from(root),
        _ => {
            eprintln!("usage: usb_probe [copiago-data-root]");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("usb_probe: could not start runtime");
            return ExitCode::FAILURE;
        }
    };

    let started = Instant::now();
    match runtime.block_on(probe_usb(&root)) {
        Ok(probe) => {
            let source = match probe.pairing_record_source {
                PairingRecordSource::Usbmuxd => "usbmuxd",
                PairingRecordSource::LocalFile => "local",
            };
            println!(
                "services={} pasteboard_advertised={} pairing_source={} elapsed_ms={}",
                probe.service_count,
                probe.pasteboard_advertised,
                source,
                started.elapsed().as_millis()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "usb_probe: {error} (elapsed_ms={})",
                started.elapsed().as_millis()
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("usb_probe requires Windows");
    ExitCode::FAILURE
}
