//! Inspect the services of one already-trusted Wi-Fi iPhone.

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    match windows::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wifi_probe: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("wifi_probe requires Windows");
    ExitCode::FAILURE
}

#[cfg(windows)]
mod windows {
    use std::env;
    use std::time::Instant;

    use copiago_core::paths::{RunMode, data_dir};
    use copiago_device::{IdentifierMigration, WifiProbeOptions, probe_wifi};

    const USAGE: &str = "usage: wifi_probe [--addr IP:PORT] [--write-identifier]";

    pub(super) fn run() -> Result<(), String> {
        let mut args = env::args_os().skip(1);
        let mut options = WifiProbeOptions::default();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--addr") if options.address.is_none() => {
                    options.address = Some(
                        args.next()
                            .and_then(|address| address.to_str()?.parse().ok())
                            .ok_or(USAGE)?,
                    );
                }
                Some("--write-identifier") if !options.write_identifier => {
                    options.write_identifier = true;
                }
                _ => return Err(USAGE.into()),
            }
        }
        if options.write_identifier && env::var_os("ICB_DATA_DIR").is_none_or(|dir| dir.is_empty())
        {
            return Err("--write-identifier requires an explicit ICB_DATA_DIR".into());
        }
        let root =
            data_dir(RunMode::Development).map_err(|_| "could not resolve data directory")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| "could not start runtime")?;

        let started = Instant::now();
        let report = runtime
            .block_on(probe_wifi(&root, options))
            .map_err(|error| format!("{error} (elapsed_ms={})", started.elapsed().as_millis()))?;
        println!(
            "pair_verify=true services={} pasteboard_advertised={} identifier_migration={:?} elapsed_ms={}",
            report.service_count,
            report.pasteboard_advertised,
            report.identifier_migration,
            started.elapsed().as_millis()
        );
        if report.identifier_migration == IdentifierMigration::WriteFailed {
            return Err("could not persist the verified identifier".into());
        }
        Ok(())
    }
}
