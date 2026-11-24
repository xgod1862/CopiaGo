//! Stream a controlled Files sample to a temporary file and verify its SHA-256.

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    match windows::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("file_bench: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("file_bench requires Windows");
    ExitCode::FAILURE
}

#[cfg(windows)]
mod windows {
    use std::{
        env, fs,
        mem::{MaybeUninit, size_of},
        os::windows::fs::OpenOptionsExt,
        path::{Path, PathBuf},
        process,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use copiago_core::paths::ensure_data_dir;
    use copiago_device::file_bench::{self, Link, Method};
    use sha2::{Digest, Sha256};
    use tokio::{
        fs::File,
        io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter, SeekFrom},
    };
    use windows_sys::Win32::{
        Storage::FileSystem::FILE_FLAG_DELETE_ON_CLOSE,
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
            Threading::GetCurrentProcess,
        },
    };

    const MIB: u64 = 1024 * 1024;
    const IO_BUFFER: usize = 128 * 1024;
    const USAGE: &str = "file_bench simulate <threshold|resolve> <100|500>\nfile_bench <wifi|usb> <threshold|resolve> <local-sample>\nICB_DATA_DIR must name a test directory under the Windows temporary directory.";

    pub(super) fn run() -> Result<(), String> {
        let mut args = env::args_os().skip(1);
        let source = args.next().ok_or(USAGE)?;
        if matches!(source.to_str(), Some("--help" | "-h")) && args.len() == 0 {
            println!("{USAGE}");
            return Ok(());
        }
        let method = match args.next().as_deref().and_then(|arg| arg.to_str()) {
            Some("threshold") => Method::Threshold,
            Some("resolve") => Method::Resolve,
            _ => return Err(USAGE.into()),
        };
        let sample = args.next().ok_or(USAGE)?;
        if args.next().is_some() {
            return Err(USAGE.into());
        }
        let source = match source.to_str() {
            Some("simulate") => match sample.to_str() {
                Some("100") => Source::Simulated(100 * MIB),
                Some("500") => Source::Simulated(500 * MIB),
                _ => return Err(USAGE.into()),
            },
            Some("wifi") => Source::Phone(Link::Wifi, sample.into()),
            Some("usb") => Source::Phone(Link::Usb, sample.into()),
            _ => return Err(USAGE.into()),
        };
        let root = test_dir()?;
        tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| "could not start runtime")?
            .block_on(run_sample(&root, source, method))
    }

    enum Source {
        Simulated(u64),
        Phone(Link, PathBuf),
    }

    async fn run_sample(root: &Path, source: Source, method: Method) -> Result<(), String> {
        let scratch = ScratchDir::create(root)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .open(scratch.0.join("payload.tmp"))
            .map_err(|_| "could not open the protected temporary output")?;
        let mut out = BufWriter::with_capacity(IO_BUFFER, File::from_std(file));

        let (source, baseline, report, expected) = match source {
            Source::Simulated(size) => {
                let baseline = memory()?;
                let (report, hash) = file_bench::simulate(method, size, &mut out)
                    .await
                    .map_err(|error| error.to_string())?;
                ("simulate", baseline, report, hash)
            }
            Source::Phone(link, sample) => {
                let mut input = File::open(&sample)
                    .await
                    .map_err(|_| "could not open the local sample")?;
                let size = input
                    .metadata()
                    .await
                    .map_err(|_| "could not stat the local sample")?
                    .len();
                if size != 100 * MIB && size != 500 * MIB {
                    return Err("the local sample must be exactly 100 MiB or 500 MiB".into());
                }
                let name = sample
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("invalid sample name")?;
                let expected = sha256(&mut input).await?;
                let source = match link {
                    Link::Wifi => "wifi",
                    Link::Usb => "usb",
                };
                let baseline = memory()?;
                let report = file_bench::receive(root, link, method, name, size, &mut out)
                    .await
                    .map_err(|error| error.to_string())?;
                (source, baseline, report, expected)
            }
        };
        let started = Instant::now();
        out.flush()
            .await
            .map_err(|_| "could not flush the output")?;
        let mut file = out.into_inner();
        file.sync_all()
            .await
            .map_err(|_| "could not sync the output")?;
        let disk_sync = started.elapsed();
        let high_water = memory()?;

        let started = Instant::now();
        let written = file
            .metadata()
            .await
            .map_err(|_| "could not stat the output")?
            .len();
        file.seek(SeekFrom::Start(0))
            .await
            .map_err(|_| "could not rewind the output")?;
        if written != report.bytes || sha256(&mut file).await? != expected {
            return Err("received sample byte count or SHA-256 differs from the original".into());
        }
        let verification = started.elapsed();
        drop(file);

        let mib_per_sec = report.bytes as f64 / MIB as f64 / report.transfer.as_secs_f64();
        println!(
            "source={source} method={method:?} bytes={} prepare_ms={:.3} transfer_ms={:.3} transfer_mib_per_sec={mib_per_sec:.2} disk_sync_ms={:.3} verify_ms={:.3} baseline_working_set_bytes={} peak_working_set_bytes={} baseline_private_bytes={} peak_commit_bytes={} sha256_match=true",
            report.bytes,
            ms(report.prepare),
            ms(report.transfer),
            ms(disk_sync),
            ms(verification),
            baseline.WorkingSetSize,
            high_water.PeakWorkingSetSize,
            baseline.PrivateUsage,
            high_water.PeakPagefileUsage,
        );
        Ok(())
    }

    fn ms(elapsed: Duration) -> f64 {
        elapsed.as_secs_f64() * 1000.0
    }

    async fn sha256(file: &mut File) -> Result<[u8; 32], &'static str> {
        let mut digest = Sha256::new();
        let mut buffer = vec![0; IO_BUFFER];
        loop {
            let count = file
                .read(&mut buffer)
                .await
                .map_err(|_| "could not read the sample for SHA-256")?;
            if count == 0 {
                return Ok(digest.finalize().into());
            }
            digest.update(&buffer[..count]);
        }
    }

    fn test_dir() -> Result<PathBuf, &'static str> {
        let root = env::var_os("ICB_DATA_DIR")
            .filter(|dir| !dir.is_empty())
            .ok_or("explicit ICB_DATA_DIR is required")?;
        let root = fs::canonicalize(root).map_err(|_| "ICB_DATA_DIR does not exist")?;
        let temp = fs::canonicalize(env::temp_dir())
            .map_err(|_| "could not resolve Windows temporary directory")?;
        if root == temp || !root.starts_with(temp) || !root.is_dir() {
            return Err(
                "ICB_DATA_DIR must be a test subdirectory of the Windows temporary directory",
            );
        }
        Ok(root)
    }

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn create(root: &Path) -> Result<Self, &'static str> {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "invalid system clock")?;
            let path = root.join(format!("file-bench-{}-{}", process::id(), stamp.as_nanos()));
            if path.exists() {
                return Err("temporary directory already exists");
            }
            ensure_data_dir(&path)
                .map_err(|_| "could not create the protected temporary directory")?;
            Ok(Self(path))
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir(&self.0);
        }
    }

    fn memory() -> Result<PROCESS_MEMORY_COUNTERS_EX, &'static str> {
        let mut counters = MaybeUninit::<PROCESS_MEMORY_COUNTERS_EX>::uninit();
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle, and the buffer
        // has the layout and size of PROCESS_MEMORY_COUNTERS_EX.
        let succeeded = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                counters.as_mut_ptr().cast(),
                size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )
        };
        if succeeded == 0 {
            return Err("could not query process memory");
        }
        // SAFETY: A successful call initialized the provided output structure.
        Ok(unsafe { counters.assume_init() })
    }
}
