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

use copiago_core::ddi::DdiStage;
use idevice::{IdeviceError, provider::RsdProvider, rsd::RsdHandshake};
use tokio::{sync::mpsc, task, time::timeout};
use tracing::{Dispatch, instrument::WithSubscriber};

use crate::download::{DownloadProgress, DownloadStats, ImageKind, load_image};

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
    Failed { stage: DdiStage, timed_out: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepError {
    Canceled,
    DeveloperModeRequired,
    Timeout,
    Kind(&'static str),
    HttpStatus(u16),
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
            IdeviceError::Timeout => Self::Timeout,
            IdeviceError::Socket(io) => io.into(),
            IdeviceError::ServiceNotFound => Self::Kind("InvalidServiceError"),
            IdeviceError::Plist(_) => Self::Kind("InvalidFileException"),
            IdeviceError::BadBuildManifest => Self::Kind("NoSuchBuildIdentityError"),
            _ => Self::Kind("IdeviceError"),
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
    use DdiStage::{Install, Sign};

    notify(&mut report, SetupUpdate::Checking)?;
    let kind = mount::image_kind(rsd);
    let started = Instant::now();
    let checked = within(INSTALL_TIMEOUT, mount::check_device(provider, rsd)).await;
    if log::finish(data_root, Install, started, checked, None, false)? {
        return Ok(());
    }

    let folder = download(data_root, kind, &mut report).await?;
    notify(&mut report, SetupUpdate::Preparing)?;

    let started = Instant::now();
    let finished = timeout(INSTALL_TIMEOUT, async {
        let Some(mut mount) = log::finish(
            data_root,
            Install,
            started,
            mount::prepare_image(provider, rsd, kind, &folder).await,
            None,
            false,
        )?
        else {
            return Ok(());
        };
        let ticket = match mount.take_cached_ticket() {
            Some(ticket) => ticket,
            None => {
                let request = log::finish(
                    data_root,
                    Install,
                    started,
                    mount.signature_request(provider, rsd).await,
                    None,
                    false,
                )?;
                notify(&mut report, SetupUpdate::Signing)?;
                let sign_started = Instant::now();
                let signed = within(SIGN_TIMEOUT, signing::request_ticket(request, kind)).await;
                log::finish(data_root, Sign, sign_started, signed, None, true)?
            }
        };
        notify(&mut report, SetupUpdate::Installing)?;
        let mounted = mount.install(provider, rsd, ticket).await;
        log::finish(data_root, Install, started, mounted, None, true)
    })
    .await;

    match finished {
        Ok(result) => result,
        Err(_) => log::finish(
            data_root,
            Install,
            started,
            Err(StepError::Timeout),
            None,
            false,
        ),
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
    data_root: &Path,
    kind: ImageKind,
    report: &mut impl FnMut(SetupUpdate) -> ControlFlow<()>,
) -> Result<PathBuf, SetupError> {
    notify(report, SetupUpdate::Downloading)?;
    let started = Instant::now();
    let cancel = CancelDownload(Arc::new(AtomicBool::new(false)));
    let worker_cancel = Arc::clone(&cancel.0);
    let worker_root = data_root.to_owned();
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
    log::finish(
        data_root,
        DdiStage::Download,
        started,
        result,
        Some(&stats),
        stats.fetched != 0,
    )
}
