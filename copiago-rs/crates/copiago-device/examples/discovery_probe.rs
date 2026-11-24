//! Read-only probe. Pass the existing device-data directory as the only argument.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match windows::run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => 2.into(),
        Err(message) => {
            eprintln!("{message}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod windows {
    use std::env;
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;
    use std::time::Instant;

    use copiago_core::dpapi;
    use copiago_device::discovery;
    use plist::Value;
    use zeroize::{Zeroize, Zeroizing};

    pub(super) fn run() -> Result<bool, &'static str> {
        let directory = env::args_os()
            .nth(1)
            .ok_or("Pass the existing device-data directory.")?;
        let irk = read_irk(Path::new(&directory))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|_| "Could not start the discovery runtime.")?;
        let start = Instant::now();
        let candidates = runtime
            .block_on(discovery::discover(irk.as_deref()))
            .map_err(|_| "Could not start DNS-SD discovery.")?;
        let matched = candidates
            .iter()
            .any(discovery::Candidate::auth_tag_matches);
        println!(
            "matched={matched} elapsed_ms={:.2}",
            start.elapsed().as_secs_f64() * 1000.0
        );
        Ok(matched)
    }

    fn read_irk(directory: &Path) -> Result<Option<Zeroizing<[u8; 16]>>, &'static str> {
        let mut files = fs::read_dir(directory)
            .map_err(|_| "Could not open the device-data directory.")?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("remote_") && name.ends_with(".plist"))
            });
        let file = files.next().ok_or("No RemotePairing record found.")?;
        if files.next().is_some() {
            return Err("Expected one RemotePairing record.");
        }
        let sealed = Zeroizing::new(
            fs::read(file.path()).map_err(|_| "Could not read the RemotePairing record.")?,
        );
        let raw = Zeroizing::new(
            dpapi::unprotect(&sealed).map_err(|_| "Could not decrypt the RemotePairing record.")?,
        );
        let mut record =
            Value::from_reader(Cursor::new(&*raw)).map_err(|_| "Invalid RemotePairing record.")?;
        let irk = if let Some(dict) = record.as_dictionary() {
            dict.get("peer_alt_irk")
                .or_else(|| dict.get("alt_irk"))
                .map(|value| {
                    value
                        .as_data()
                        .and_then(|data| <&[u8; 16]>::try_from(data).ok())
                        .map(|key| Zeroizing::new(*key))
                        .ok_or("Invalid RemotePairing IRK.")
                })
                .transpose()
        } else {
            Err("Invalid RemotePairing record.")
        };
        scrub_record(&mut record);
        irk
    }

    fn scrub_record(value: &mut Value) {
        match value {
            Value::Data(bytes) => bytes.zeroize(),
            Value::String(text) => text.zeroize(),
            Value::Array(items) => items.iter_mut().for_each(scrub_record),
            Value::Dictionary(items) => items.values_mut().for_each(scrub_record),
            _ => {}
        }
    }
}
