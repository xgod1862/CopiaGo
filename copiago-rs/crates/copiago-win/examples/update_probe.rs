//! Explicit, read-only WinHTTP smoke check against the actual CopiaGo release API.

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    use copiago_win::update::{STARTUP_DELAY, UpdateCheck};
    use std::{
        env, io,
        sync::mpsc,
        time::{Duration, Instant},
    };

    let mut args = env::args().skip(1);
    let usage = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: update_probe --github <installed version> [--open]",
        )
    };
    if args.next().as_deref() != Some("--github") {
        return Err(usage());
    }
    let current = args.next().ok_or_else(usage)?;
    let open = match args.next().as_deref() {
        None => false,
        Some("--open") if args.next().is_none() => true,
        _ => return Err(usage()),
    };
    let (send, wake) = mpsc::channel();
    let started = Instant::now();
    let mut check = UpdateCheck::start(&current, true, move || {
        let _ = send.send(());
    });
    wake.recv_timeout(STARTUP_DELAY + Duration::from_secs(12))
        .map_err(io::Error::other)?;
    println!(
        "installed={current} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    if let Some(release) = check.release() {
        println!("available={} page={}", release.version(), release.page());
        if open {
            use windows::Win32::System::Com::{
                COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
            };
            // SAFETY: Initializes the current thread for shell execution, with no reserved data.
            let initialized =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            initialized
                .ok()
                .map_err(|error| io::Error::other(error.to_string()))?;
            let result = release.open();
            // SAFETY: Balances the successful COM initialization on the same thread.
            unsafe { CoUninitialize() };
            result?;
            println!("opened=true");
        }
    } else {
        println!("available=none");
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
