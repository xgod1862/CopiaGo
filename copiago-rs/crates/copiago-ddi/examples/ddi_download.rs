//! Download one pinned image set into an explicit ICB_DATA_DIR for acceptance testing.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::env;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    use copiago_core::paths::{self, RunMode};
    use copiago_ddi::download::{DownloadStats, ImageKind, load_image};

    let kind = match env::args().nth(1).as_deref() {
        Some("cryptex") => ImageKind::Cryptex,
        Some("personalized") => ImageKind::Personalized,
        _ => return Err("usage: ddi_download cryptex|personalized".into()),
    };
    if env::var_os("ICB_DATA_DIR").is_none_or(|value| value.is_empty()) {
        return Err("set ICB_DATA_DIR to a temporary data directory".into());
    }
    let data_root = paths::data_dir(RunMode::Development)?;
    let mut stats = DownloadStats::default();
    let started = Instant::now();
    let result = load_image(
        &data_root,
        kind,
        &AtomicBool::new(false),
        &mut stats,
        |_| {},
    );
    println!(
        "{kind:?} elapsed_ms={} fetched={} resumed={} source={:?} failures={:?}",
        started.elapsed().as_millis(),
        stats.fetched,
        stats.resumed,
        stats.source,
        stats.failures,
    );
    println!("{}", result?.display());
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("DDI download requires Windows");
}
