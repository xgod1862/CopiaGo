//! DDI routines over idevice's authenticated RSD and RemoteXPC transport.
//!
//! Protocol and asset handling reference idevice 0.1.68 (MIT, ../idevice-LICENSE.txt).
//! The upstream cryptexd feature enables the unused general-purpose TSS trust bundle.

use std::{borrow::Cow, path::Path};

use idevice::{
    IdeviceError, ReadWrite, RemoteXpcClient, RsdService,
    provider::RsdProvider,
    rsd::RsdHandshake,
    xpc::{Dictionary, XPCObject},
};
use plist::Value;
use tokio::fs;

use super::{CRYPTEX_SERVICE, StepError, mapped_image::MappedImage, signing};

const DDI_NAME: &str = "com.apple.MobileAsset.DDI";
const PROTOCOL_ERROR: StepError = StepError::Kind("CryptexdError");
const MANIFEST_ERROR: StepError = StepError::Kind("NoSuchBuildIdentityError");

struct Client(RemoteXpcClient<Box<dyn ReadWrite>>);

impl RsdService for Client {
    fn rsd_service_name() -> Cow<'static, str> {
        Cow::Borrowed(CRYPTEX_SERVICE)
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        let mut client = RemoteXpcClient::new(stream).await?;
        client.do_handshake().await?;
        Ok(Self(client))
    }
}

impl Client {
    async fn send(&mut self, routine: &str, args: Dictionary) -> Result<(), IdeviceError> {
        self.0
            .send_object(
                idevice::xpc!({ "routine": routine, "argv": XPCObject::Dictionary(args) }),
                true,
            )
            .await
    }

    async fn reply(&mut self) -> Result<Option<plist::Dictionary>, StepError> {
        let mut reply = self
            .0
            .recv()
            .await?
            .into_dictionary()
            .ok_or(PROTOCOL_ERROR)?;
        if reply.get("cferr").and_then(Value::as_dictionary).is_some()
            || reply.get("error").is_some_and(|code| {
                code.as_signed_integer() != Some(0) && code.as_unsigned_integer() != Some(0)
            })
        {
            return Err(PROTOCOL_ERROR);
        }
        Ok(reply.remove("argv").and_then(Value::into_dictionary))
    }

    async fn query(
        mut self,
        routine: &str,
        args: Dictionary,
    ) -> Result<Option<plist::Dictionary>, StepError> {
        self.send(routine, args).await?;
        self.reply().await
    }
}

pub(super) async fn installed_version(
    provider: &mut impl RsdProvider,
    rsd: &mut RsdHandshake,
) -> Result<Option<String>, StepError> {
    let reply = Client::connect_rsd(provider, rsd)
        .await?
        .query("copy-installed", Dictionary::new())
        .await?;
    let entries = reply
        .as_ref()
        .and_then(|argv| argv.get("remote-cryptex-array"))
        .and_then(Value::as_array);
    let ddi = entries
        .into_iter()
        .flatten()
        .filter_map(Value::as_dictionary)
        .find(|entry| {
            entry
                .get("remote-cryptex-identifier")
                .and_then(Value::as_string)
                == Some(DDI_NAME)
        });
    ddi.map(|entry| {
        entry
            .get("remote-cryptex-version")
            .and_then(Value::as_string)
            .filter(|version| !version.is_empty())
            .map(str::to_owned)
            .ok_or(PROTOCOL_ERROR)
    })
    .transpose()
}

pub(super) async fn uninstall_ddi(
    provider: &mut impl RsdProvider,
    rsd: &mut RsdHandshake,
    version: &str,
) -> Result<(), StepError> {
    let args = Dictionary::from([
        (
            "remote-cryptex-identifier".into(),
            XPCObject::String(DDI_NAME.to_owned()),
        ),
        (
            "remote-cryptex-version".into(),
            XPCObject::String(version.to_owned()),
        ),
    ]);
    Client::connect_rsd(provider, rsd)
        .await?
        .query("uninstall", args)
        .await?;
    Ok(())
}

pub(in crate::setup) struct Cryptex {
    image: MappedImage,
    trust_cache: Vec<u8>,
    info: Vec<u8>,
    root_hash: Vec<u8>,
    identity: plist::Dictionary,
}

impl Cryptex {
    pub(super) async fn load(folder: &Path) -> Result<Self, StepError> {
        let bytes = fs::read(folder.join("BuildManifest.plist")).await?;
        let manifest: plist::Dictionary = plist::from_bytes(&bytes).map_err(IdeviceError::from)?;
        let identity = manifest
            .get("BuildIdentities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_dictionary)
            .find(|identity| {
                identity
                    .get("Info")
                    .and_then(Value::as_dictionary)
                    .and_then(|info| info.get("Variant"))
                    .and_then(Value::as_string)
                    .is_some_and(|variant| variant.ends_with("Developer Disk Image Cryptex"))
            })
            .ok_or(MANIFEST_ERROR)?
            .clone();
        // idevice's file transfer takes a copy. Map the source image to avoid two heap copies.
        let image = MappedImage::open(&folder.join("Image.dmg"))?;
        Ok(Self {
            image,
            trust_cache: fs::read(folder.join("Image.dmg.trustcache")).await?,
            info: fs::read(folder.join("Image.dmg.cryptex_info")).await?,
            root_hash: fs::read(folder.join("Image.dmg.root_hash")).await?,
            identity,
        })
    }

    pub(super) async fn signature_request(
        &self,
        provider: &mut impl RsdProvider,
        rsd: &mut RsdHandshake,
    ) -> Result<plist::Dictionary, StepError> {
        let domain = self.number("Cryptex1,NonceDomain")?;
        let identifiers = Client::connect_rsd(provider, rsd)
            .await?
            .query("read-personalization-id", Dictionary::new())
            .await?
            .ok_or(PROTOCOL_ERROR)?;
        let nonce = Client::connect_rsd(provider, rsd)
            .await?
            .query(
                "get-nonce",
                Dictionary::from([("nonce-domain-handle".into(), XPCObject::UInt64(domain))]),
            )
            .await?
            .and_then(|mut reply| reply.remove("nonce"))
            .and_then(Value::into_data)
            .ok_or(PROTOCOL_ERROR)?;
        let (wrapped, len) = nonce.split_last_chunk::<4>().ok_or(PROTOCOL_ERROR)?;
        let length = u32::from_le_bytes(*len) as usize;
        let nonce = wrapped
            .get(2..)
            .and_then(|bytes| bytes.get(..length))
            .ok_or(PROTOCOL_ERROR)?;
        signing::cryptex_request(&self.identity, &identifiers, nonce)
    }

    pub(super) async fn install(
        self,
        provider: &mut impl RsdProvider,
        rsd: &mut RsdHandshake,
        ticket: &[u8],
    ) -> Result<(), StepError> {
        let payloads = [
            ("image", self.image.bytes()),
            ("trustcache", self.trust_cache.as_slice()),
            ("im4m", ticket),
            ("info", self.info.as_slice()),
            ("volumehash", self.root_hash.as_slice()),
        ];
        // Use the separate DDI persistence values. idevice 0.1.68's higher-level helper
        // writes nonce_persistence into both fields.
        let mut args = Dictionary::from([
            ("auth".into(), XPCObject::UInt64(0)),
            ("client-version".into(), XPCObject::UInt64(3)),
            (
                "cryptex1-properties".into(),
                XPCObject::Dictionary(self.mount_properties()?),
            ),
            ("image-type-index".into(), XPCObject::Int64(10)),
            ("persistence".into(), XPCObject::UInt64(2)),
            ("nonce-persistence".into(), XPCObject::UInt64(1)),
        ]);
        for (index, (name, bytes)) in payloads.iter().enumerate() {
            let size = Dictionary::from([("s".into(), XPCObject::UInt64(bytes.len() as u64))]);
            args.insert(
                (*name).into(),
                XPCObject::FileTransfer {
                    msg_id: index as u64 + 1,
                    data: Box::new(XPCObject::Dictionary(size)),
                },
            );
        }
        let mut client = Client::connect_rsd(provider, rsd).await?;
        client.send("install", args).await?;
        for (index, (_, bytes)) in payloads.into_iter().enumerate() {
            client.0.send_file_transfer(index as u64 + 1, bytes).await?;
        }
        client.reply().await?;
        Ok(())
    }

    fn number(&self, key: &str) -> Result<u64, StepError> {
        self.identity
            .get(key)
            .and_then(Value::as_unsigned_integer)
            .ok_or(MANIFEST_ERROR)
    }

    fn mount_properties(&self) -> Result<Dictionary, StepError> {
        let mut properties = Dictionary::from([("MountedCryptex".into(), XPCObject::Bool(false))]);
        for key in ["Cryptex1,SubType", "Cryptex1,NonceDomain"] {
            properties.insert(key.into(), XPCObject::UInt64(self.number(key)?));
        }
        for (key, name) in [
            ("Cryptex1,UseProductClass", "Cryptex1,UseProductClass"),
            ("Cryptex1,Version", "Cryptex1,Version"),
            (
                "Cryptex1,PreauthorizationVersion",
                "Cryptex1,PreauthVersion",
            ),
        ] {
            let value = self.identity.get(key).ok_or(MANIFEST_ERROR)?;
            properties.insert(name.into(), XPCObject::from(value.clone()));
        }
        Ok(properties)
    }
}
