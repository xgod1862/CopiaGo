//! Prepare Apple-signed developer services over an already authenticated RSD tunnel.

mod log;
mod mount;
mod signing;
#[cfg(test)]
mod tests;

use std::{
    future::Future,
    io,
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use copiago_core::ddi::{DdiCause, DdiStage};
use idevice::{IdeviceError, provider::RsdProvider, rsd::RsdHandshake};
use tokio::{sync::mpsc, task, time::timeout};
use tracing::{Dispatch, instrument::WithSubscriber};

use crate::download::{DownloadProgress, DownloadStats, ImageKind, load_image};
use log::SetupLog;

const SIGN_TIMEOUT: Duration = Duration::from_secs(45);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(240);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupUpdate {
    Checking,
    Downloading,
    Download(DownloadProgress),
    Preparing,
    Signing,
    Installing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupError {
    Canceled,
    DeveloperModeRequired,
    Failed {
        stage: DdiStage,
        timed_out: bool,
        cause: DdiCause,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StepError {
    Canceled,
    DeveloperModeRequired,
    Timeout,
    Kind(&'static str),
    HttpStatus(u16),
    /// A device reply without a dedicated kind. `detail` is already scrubbed of identifiers.
    Device {
        kind: &'static str,
        detail: String,
    },
}

impl From<IdeviceError> for StepError {
    fn from(error: IdeviceError) -> Self {
        match error {
            IdeviceError::DeveloperModeNotEnabled => Self::DeveloperModeRequired,
            IdeviceError::InternalError(detail)
                if detail.contains("Developer mode is not enabled") =>
            {
                Self::DeveloperModeRequired
            }
            IdeviceError::DeviceLocked => Self::Kind("PasswordRequiredError"),
            IdeviceError::InternalError(detail) => Self::Device {
                kind: "InternalError",
                detail: log::scrub(&detail),
            },
            IdeviceError::Timeout => Self::Timeout,
            IdeviceError::Socket(io) => io.into(),
            IdeviceError::ServiceNotFound => Self::Kind("InvalidServiceError"),
            IdeviceError::Plist(_) => Self::Kind("InvalidFileException"),
            IdeviceError::BadBuildManifest => Self::Kind("NoSuchBuildIdentityError"),
            // Keep only the variant name: payloads may hold identifiers or ticket bytes.
            other => Self::Device {
                kind: "IdeviceError",
                detail: log::variant_name(&other),
            },
        }
    }
}

impl From<io::Error> for StepError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::TimedOut => Self::Timeout,
            io::ErrorKind::NotFound => Self::Kind("FileNotFoundError"),
            io::ErrorKind::PermissionDenied => Self::Kind("PermissionError"),
            io::ErrorKind::ConnectionRefused => Self::Kind("ConnectionRefusedError"),
            _ => Self::Kind("OSError"),
        }
    }
}

/// Installs the required image if it is absent. The caller must refresh the RSD tunnel afterwards.
/// Dropping this future cancels device operations and signals the blocking downloader to stop.
pub async fn prepare_developer_services(
    provider: &mut impl RsdProvider,
    rsd: &mut RsdHandshake,
    data_root: &Path,
    report: impl FnMut(SetupUpdate) -> ControlFlow<()>,
) -> Result<(), SetupError> {
    // idevice's trace output can contain personalization identifiers and raw ticket bytes.
    prepare(provider, rsd, data_root, report)
        .with_subscriber(Dispatch::none())
        .await
}

async fn prepare(
    provider: &mut impl RsdProvider,
    rsd: &mut RsdHandshake,
    data_root: &Path,
    mut report: impl FnMut(SetupUpdate) -> ControlFlow<()>,
) -> Result<(), SetupError> {
    notify(&mut report, SetupUpdate::Checking)?;
    let kind = mount::image_kind(rsd);
    log::context(rsd, kind);
    let mut log = SetupLog::new(data_root);
    let started = Instant::now();
    let checked = within(
        INSTALL_TIMEOUT,
        mount::check_device(provider, rsd, &mut log),
    )
    .await;
    if log.finish(DdiStage::Install, started, checked, None, false)? {
        return Ok(());
    }

    log.step = "LoadDeveloperImage";
    let folder = download(&log, kind, &mut report).await?;
    install_image(
        provider,
        rsd,
        kind,
        &folder,
        &mut log,
        report,
        signing::request_ticket,
    )
    .await
}

async fn install_image(
    provider: &mut impl RsdProvider,
    rsd: &mut RsdHandshake,
    kind: ImageKind,
    folder: &Path,
    log: &mut SetupLog<'_>,
    mut report: impl FnMut(SetupUpdate) -> ControlFlow<()>,
    request_ticket: impl AsyncFnOnce(plist::Dictionary, ImageKind) -> Result<Vec<u8>, StepError>,
) -> Result<(), SetupError> {
    use DdiStage::{Install, Sign};

    notify(&mut report, SetupUpdate::Preparing)?;

    let started = Instant::now();
    let finished = timeout(INSTALL_TIMEOUT, async {
        let prepared = mount::prepare_image(provider, rsd, kind, folder, log).await;
        let Some(mut mount) = log.finish(Install, started, prepared, None, false)? else {
            return Ok(());
        };
        let ticket = match mount.take_cached_ticket() {
            Some(ticket) => ticket,
            None => {
                let request = mount.signature_request(provider, rsd, log).await;
                let request = log.finish(Install, started, request, None, false)?;
                notify(&mut report, SetupUpdate::Signing)?;
                log.step = "RequestSigningTicket";
                let sign_started = Instant::now();
                let signed = within(SIGN_TIMEOUT, request_ticket(request, kind)).await;
                log.finish(Sign, sign_started, signed, None, true)?
            }
        };
        notify(&mut report, SetupUpdate::Installing)?;
        let mounted = mount.install(provider, rsd, ticket, log).await;
        log.finish(Install, started, mounted, None, true)
    })
    .await;

    match finished {
        Ok(result) => result,
        Err(_) => log.finish(Install, started, Err(StepError::Timeout), None, false),
    }
}

fn notify(
    report: &mut impl FnMut(SetupUpdate) -> ControlFlow<()>,
    update: SetupUpdate,
) -> Result<(), SetupError> {
    match report(update) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(()) => Err(SetupError::Canceled),
    }
}

async fn within<T>(
    limit: Duration,
    operation: impl Future<Output = Result<T, StepError>>,
) -> Result<T, StepError> {
    timeout(limit, operation)
        .await
        .unwrap_or(Err(StepError::Timeout))
}

struct CancelDownload(Arc<AtomicBool>);

impl Drop for CancelDownload {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

async fn download(
    log: &SetupLog<'_>,
    kind: ImageKind,
    report: &mut impl FnMut(SetupUpdate) -> ControlFlow<()>,
) -> Result<PathBuf, SetupError> {
    notify(report, SetupUpdate::Downloading)?;
    let started = Instant::now();
    let cancel = CancelDownload(Arc::new(AtomicBool::new(false)));
    let worker_cancel = Arc::clone(&cancel.0);
    let worker_root = log.data_root.to_owned();
    let (updates, mut progress) = mpsc::channel(16);
    let worker = task::spawn_blocking(move || {
        let mut stats = DownloadStats::default();
        let result = load_image(&worker_root, kind, &worker_cancel, &mut stats, |event| {
            let _ = updates.try_send(event);
        });
        (result, stats)
    });
    while let Some(event) = progress.recv().await {
        notify(report, SetupUpdate::Download(event))?;
    }
    let (result, stats) = match worker.await {
        Ok((result, stats)) => (result.map_err(log::download_error), stats),
        Err(_) => (
            Err(StepError::Kind("DownloadWorkerError")),
            DownloadStats::default(),
        ),
    };
    log.finish(
        DdiStage::Download,
        started,
        result,
        Some(&stats),
        stats.fetched != 0,
    )
}
