use std::path::Path;

use idevice::{
    IdeviceError, RsdService, mobile_image_mounter::ImageMounter, provider::RsdProvider,
    rsd::RsdHandshake,
};
use plist::Value;
use sha2::{Digest, Sha384};
use tokio::fs;

use super::{SetupLog, StepError, signing};
use crate::download::ImageKind;

mod cryptex;
mod mapped_image;

use cryptex::{Cryptex, installed_version, uninstall_ddi};

pub(super) const CRYPTEX_SERVICE: &str = "com.apple.security.cryptexd.remote";
const MOUNTER_IMAGE: &str = "Personalized";
const PERSONALIZATION_TYPE: &str = "DeveloperDiskImage";
const MISSING_CACHED_MANIFEST: &str =
    "Failed to find a cached manifest (im4m) for variant DeveloperDiskImage";

pub(super) fn image_kind(rsd: &RsdHandshake) -> ImageKind {
    // CopiaGo's pinned Python stack selects Cryptex for iOS 27 and newer.
    // An old service table may omit Features; a nonempty list is authoritative.
    let version = rsd
        .properties
        .get("OSVersion")
        .and_then(Value::as_string)
        .and_then(|version| version.split('.').next()?.parse::<u32>().ok());
    if version.is_some_and(|major| major >= 27) && cryptex_supports(rsd, "CryptexInstall") {
        ImageKind::Cryptex
    } else {
        ImageKind::Personalized
    }
}

fn cryptex_supports(rsd: &RsdHandshake, feature: &str) -> bool {
    rsd.services.get(CRYPTEX_SERVICE).is_some_and(|service| {
        service.features.as_ref().is_none_or(|features| {
            features.is_empty() || features.iter().any(|name| name == feature)
        })
    })
}

pub(super) async fn check_device(
    provider: &mut impl RsdProvider,
    rsd: &mut RsdHandshake,
    log: &mut SetupLog<'_>,
) -> Result<bool, StepError> {
    let mut mounter = ImageMounter::connect_rsd(provider, rsd).await?;
    check_mounter(&mut mounter, log).await
}

async fn check_mounter(
    mounter: &mut ImageMounter,
    log: &mut SetupLog<'_>,
) -> Result<bool, StepError> {
    if personalized_mounted(mounter, log).await? {
        return Ok(true);
    }
    log.step = "QueryDeveloperModeStatus";
    if !mounter.query_developer_mode_status().await? {
        return Err(StepError::DeveloperModeRequired);
    }
    Ok(false)
}

async fn personalized_mounted(
    mounter: &mut ImageMounter,
    log: &mut SetupLog<'_>,
) -> Result<bool, IdeviceError> {
    log.step = "LookupImage";
    match mounter.lookup_image(MOUNTER_IMAGE).await {
        Ok(signature) if !signature.is_empty() => return Ok(true),
        Ok(_) | Err(IdeviceError::NotFound | IdeviceError::ImageNotMounted) => {}
        Err(error) => return Err(error),
    }
    // Some iOS versions list an installed image only in CopyDevices.
    log.step = "CopyDevices";
    Ok(mounter.copy_devices().await?.iter().any(|image| {
        image
            .as_dictionary()
            .and_then(|image| image.get("DiskImageType"))
            .and_then(Value::as_string)
            == Some(MOUNTER_IMAGE)
    }))
}

pub(super) struct Personalized {
    mounter: ImageMounter,
    cached_ticket: Option<Vec<u8>>,
    image: Vec<u8>,
    trust_cache: Vec<u8>,
    build_manifest: Option<plist::Dictionary>,
}

pub(super) enum PreparedImage {
    Cryptex(Cryptex),
    Personalized(Personalized),
}

pub(super) async fn prepare_image(
    provider: &mut impl RsdProvider,
    rsd: &mut RsdHandshake,
    kind: ImageKind,
    folder: &Path,
    log: &mut SetupLog<'_>,
) -> Result<Option<PreparedImage>, StepError> {
    log.step = "ConnectImageMounter";
    let mut mounter = ImageMounter::connect_rsd(provider, rsd).await?;
    if check_mounter(&mut mounter, log).await? {
        return Ok(None);
    }
    if kind == ImageKind::Cryptex {
        log.step = "CheckCryptexFeatures";
        if !cryptex_supports(rsd, "ReadIdentifiers") {
            return Err(StepError::Kind("DeviceFeatureNotSupportedError"));
        }
        drop(mounter);
        log.step = "LoadCryptexAssets";
        let assets = Cryptex::load(folder).await?;
        log.step = "CopyInstalledCryptex";
        if let Some(version) = installed_version(provider, rsd).await? {
            // copy-installed can list a DDI whose image is no longer mounted.
            // Scope removal to the DDI entry and version, then query the signing nonce.
            if check_personalized_mount(provider, rsd, log).await? {
                return Ok(None);
            }
            log.step = "UninstallCryptex";
            uninstall_ddi(provider, rsd, &version).await?;
        }
        return Ok(Some(PreparedImage::Cryptex(assets)));
    }
    log.step = "ReadDeveloperImage";
    let image = fs::read(folder.join("Image.dmg")).await?;
    log.step = "ReadTrustCache";
    let trust_cache = fs::read(folder.join("Image.trustcache")).await?;
    let digest = Sha384::digest(&image).to_vec();
    log.step = "QueryPersonalizationManifest";
    let cached_ticket = match mounter
        .query_personalization_manifest(PERSONALIZATION_TYPE, digest)
        .await
    {
        Ok(ticket) if !ticket.is_empty() => Some(ticket),
        Ok(_) | Err(IdeviceError::NotFound | IdeviceError::ImageNotMounted) => None,
        // The user must act on these; a fresh ticket would hit the same wall.
        Err(error @ (IdeviceError::DeviceLocked | IdeviceError::DeveloperModeNotEnabled)) => {
            return Err(error.into());
        }
        // A cached ticket only saves a signing round trip, so any other reply falls back
        // to Apple signing. iOS 18.5 reports a plain cache miss as an InternalError, and
        // later releases may word it differently; a real fault resurfaces at upload/mount.
        Err(error) => {
            let (response, detail) = match error {
                IdeviceError::InternalError(detail) if detail.contains(MISSING_CACHED_MANIFEST) => {
                    ("InternalError", "cached_manifest_missing".to_owned())
                }
                IdeviceError::InternalError(detail) => {
                    ("InternalError", super::log::scrub(&detail))
                }
                other => ("IdeviceError", super::log::variant_name(&other)),
            };
            copiago_core::diagnostic::event(format_args!(
                "event=setup_manifest_cache_miss response={response} detail=\"{detail}\""
            ));
            None
        }
    };
    let build_manifest = if cached_ticket.is_none() {
        // A cache miss may close the service socket.
        log.step = "ReconnectImageMounter";
        mounter = ImageMounter::connect_rsd(provider, rsd).await?;
        log.step = "ReadBuildManifest";
        let bytes = fs::read(folder.join("BuildManifest.plist")).await?;
        log.step = "ParseBuildManifest";
        Some(plist::from_bytes(&bytes).map_err(IdeviceError::from)?)
    } else {
        None
    };
    Ok(Some(PreparedImage::Personalized(Personalized {
        mounter,
        cached_ticket,
        image,
        trust_cache,
        build_manifest,
    })))
}

impl PreparedImage {
    pub(super) fn take_cached_ticket(&mut self) -> Option<Vec<u8>> {
        match self {
            Self::Cryptex(_) => None,
            Self::Personalized(image) => image.cached_ticket.take(),
        }
    }

    pub(super) async fn signature_request(
        &mut self,
        provider: &mut impl RsdProvider,
        rsd: &mut RsdHandshake,
        log: &mut SetupLog<'_>,
    ) -> Result<plist::Dictionary, StepError> {
        match self {
            Self::Cryptex(assets) => {
                log.step = "PrepareCryptexSignature";
                assets.signature_request(provider, rsd).await
            }
            Self::Personalized(image) => {
                log.step = "LoadBuildIdentity";
                let manifest = image
                    .build_manifest
                    .take()
                    .ok_or(StepError::Kind("InvalidFileException"))?;
                log.step = "ReadUniqueChipID";
                let ecid = rsd
                    .properties
                    .get("UniqueChipID")
                    .and_then(Value::as_unsigned_integer)
                    .ok_or(StepError::Kind("MissingPersonalizationIdentifier"))?;
                log.step = "QueryPersonalizationIdentifiers";
                let ids = image
                    .mounter
                    .query_personalization_identifiers(None)
                    .await?;
                log.step = "QueryNonce";
                let nonce = image
                    .mounter
                    .query_nonce(Some(PERSONALIZATION_TYPE))
                    .await?;
                log.step = "BuildPersonalizationRequest";
                signing::personalized_request(&manifest, &ids, ecid, nonce)
            }
        }
    }

    pub(super) async fn install(
        self,
        provider: &mut impl RsdProvider,
        rsd: &mut RsdHandshake,
        ticket: Vec<u8>,
        log: &mut SetupLog<'_>,
    ) -> Result<(), StepError> {
        match self {
            Self::Cryptex(assets) => {
                log.step = "InstallCryptex";
                if let Err(error) = assets.install(provider, rsd, &ticket).await {
                    return if check_personalized_mount(provider, rsd, log)
                        .await
                        .unwrap_or(false)
                    {
                        Ok(())
                    } else {
                        log.step = "InstallCryptex";
                        Err(error)
                    };
                }
                log.step = "CopyInstalledCryptex";
                installed_version(provider, rsd)
                    .await?
                    .ok_or(StepError::Kind("CryptexdError"))?;
                Ok(())
            }
            Self::Personalized(mut image) => {
                log.step = "UploadImage";
                image
                    .mounter
                    .upload_image(MOUNTER_IMAGE, &image.image, ticket.clone())
                    .await?;
                log.step = "MountImage";
                let result = image
                    .mounter
                    .mount_image(MOUNTER_IMAGE, ticket, Some(image.trust_cache), None)
                    .await;
                if let Err(ref error) = result
                    && (is_already_mounted(error)
                        || check_personalized_mount(provider, rsd, log)
                            .await
                            .unwrap_or(false))
                {
                    return Ok(());
                }
                log.step = "MountImage";
                Ok(result?)
            }
        }
    }
}

fn is_already_mounted(error: &IdeviceError) -> bool {
    matches!(error, IdeviceError::InternalError(detail) if detail.contains("is already mounted"))
}

async fn check_personalized_mount(
    provider: &mut impl RsdProvider,
    rsd: &mut RsdHandshake,
    log: &mut SetupLog<'_>,
) -> Result<bool, IdeviceError> {
    log.step = "ConnectImageMounter";
    personalized_mounted(&mut ImageMounter::connect_rsd(provider, rsd).await?, log).await
}
