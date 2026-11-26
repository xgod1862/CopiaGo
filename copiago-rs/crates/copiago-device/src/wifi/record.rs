use std::ffi::OsStr;
use std::fs;
use std::io::{self, Cursor, ErrorKind};
use std::path::{Path, PathBuf};
use std::ptr;

use copiago_core::{DeviceInfo, atomic, dpapi, paths::DEVICE_DATA_DIR};
use ed25519_dalek::{SigningKey, VerifyingKey};
use idevice::remote_pairing::RpPairingFile;
use plist::{Dictionary, Value};
use uuid::Uuid;
use windows_sys::Win32::System::SystemInformation::{
    ComputerNamePhysicalDnsHostname, GetComputerNameExW,
};
use zeroize::{Zeroize, Zeroizing};

use super::{IdentifierMigration, WifiProbeError};

pub(crate) struct PairedDevice {
    pub(crate) serial: String,
    pub(crate) path: PathBuf,
}

/// How the UDID a phone reports through RSD compares with the paired record's serial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RsdIdentity {
    Same,
    /// The handshake did not include `UniqueDeviceID`.
    Absent,
    /// Equal once `-` separators and letter case are ignored.
    SeparatorOnly,
    Different,
}

impl PairedDevice {
    /// Diagnostic fields comparing the RSD-reported UDID with the saved serial.
    ///
    /// The result is for the log only and must never reject a tunnel. By the time RSD
    /// answers, the phone was already selected by this serial (usbmux routes USB
    /// connections by it) or proven over Wi-Fi by an IRK-matched advertisement plus
    /// pair-verify with this PC's per-pairing key. A mismatch therefore means the two
    /// sources format the UDID differently, as issue #21 showed: rejecting on it blocked
    /// a working connection on every attempt and told the user to re-plug the cable.
    ///
    /// Only lengths and separator counts are logged, because users paste the log into
    /// public issues.
    pub(crate) fn rsd_identity_fields(&self, rsd: &idevice::rsd::RsdHandshake) -> String {
        let reported = rsd
            .properties
            .get("UniqueDeviceID")
            .and_then(Value::as_string);
        identity_fields(&self.serial, reported)
    }
}

fn identity_fields(saved: &str, reported: Option<&str>) -> String {
    match (compare_udid(saved, reported), reported) {
        (identity @ (RsdIdentity::SeparatorOnly | RsdIdentity::Different), Some(reported)) => {
            format!(
                "identity={identity:?} saved={} reported={}",
                udid_shape(saved),
                udid_shape(reported)
            )
        }
        (identity, _) => format!("identity={identity:?}"),
    }
}

fn compare_udid(saved: &str, reported: Option<&str>) -> RsdIdentity {
    let Some(reported) = reported else {
        return RsdIdentity::Absent;
    };
    if reported.eq_ignore_ascii_case(saved) {
        return RsdIdentity::Same;
    }
    let compact = |udid: &str| {
        udid.chars()
            .filter(|character| *character != '-')
            .map(|character| character.to_ascii_uppercase())
            .collect::<String>()
    };
    if compact(reported) == compact(saved) {
        RsdIdentity::SeparatorOnly
    } else {
        RsdIdentity::Different
    }
}

/// `len<N>/dash<M>`: enough to tell formats apart without exposing the identifier.
fn udid_shape(udid: &str) -> String {
    let dashes = udid.chars().filter(|character| *character == '-').count();
    format!("len{}/dash{dashes}", udid.chars().count())
}

pub(super) struct RemoteRecord {
    pub(super) pairing: RpPairingFile,
    path: PathBuf,
    original: Zeroizing<Vec<u8>>,
    fields: SensitivePlist,
    needs_identifier: bool,
}

impl RemoteRecord {
    pub(super) fn read(path: &Path) -> Result<Self, WifiProbeError> {
        let original =
            Zeroizing::new(fs::read(path).map_err(|_| WifiProbeError::ReadPairingRecord)?);
        let decrypted;
        let plaintext = if original.starts_with(dpapi::MAGIC) {
            decrypted = Zeroizing::new(
                dpapi::unprotect(&original).map_err(|_| WifiProbeError::ReadPairingRecord)?,
            );
            &*decrypted
        } else {
            &*original
        };
        let fields = SensitivePlist(
            Value::from_reader(Cursor::new(plaintext))
                .map_err(|_| WifiProbeError::InvalidPairingRecord)?,
        );
        let dict = fields
            .0
            .as_dictionary()
            .ok_or(WifiProbeError::InvalidPairingRecord)?;

        let (identifier, needs_identifier) = record_identifier(dict, windows_hostname)?;
        let public_key = VerifyingKey::from_bytes(record_key(dict, "public_key")?)
            .map_err(|_| WifiProbeError::InvalidPairingRecord)?;
        let private_key = SigningKey::from_bytes(record_key(dict, "private_key")?);
        let pairing = RpPairingFile {
            e_private_key: private_key,
            e_public_key: public_key,
            identifier,
            // The IRK stays in the plist and is used only for mDNS matching.
            alt_irk: None,
        };
        Ok(Self {
            pairing,
            path: path.to_owned(),
            original,
            fields,
            needs_identifier,
        })
    }

    pub(super) fn peer_alt_irk(&self) -> Option<&[u8; 16]> {
        self.fields
            .0
            .as_dictionary()?
            .get("peer_alt_irk")?
            .as_data()?
            .try_into()
            .ok()
    }

    pub(super) fn persist_identifier(&mut self, write: bool) -> IdentifierMigration {
        if !self.needs_identifier {
            return IdentifierMigration::AlreadyPresent;
        }
        if !write {
            return IdentifierMigration::ReadOnly;
        }
        if self.write_identifier().is_ok() {
            self.needs_identifier = false;
            IdentifierMigration::Written
        } else {
            IdentifierMigration::WriteFailed
        }
    }

    fn write_identifier(&mut self) -> Result<(), WifiProbeError> {
        let fields = self
            .fields
            .0
            .as_dictionary_mut()
            .ok_or(WifiProbeError::InvalidPairingRecord)?;
        fields.insert(
            "identifier".into(),
            Value::String(self.pairing.identifier().to_owned()),
        );
        let sealed = self
            .fields
            .seal()
            .map_err(|_| WifiProbeError::ReadPairingRecord)?;

        atomic::replace_existing_if_unchanged(&self.path, &self.original, &sealed)
            .map_err(|_| WifiProbeError::ReadPairingRecord)
    }
}

/// Seals a new wireless pairing record. It holds only what reconnecting needs: CopiaGo
/// never asks the phone for a remote unlock key, so none is stored.
pub(crate) fn save_remote_record(path: &Path, mut pairing: RpPairingFile) -> io::Result<()> {
    // idevice names the value alt_irk. CopiaGo and Python match ads using peer_alt_irk.
    let mut irk = Zeroizing::new(pairing.alt_irk.take().unwrap_or_default());
    if irk.len() != 16 {
        return Err(io::Error::new(ErrorKind::InvalidData, "missing peer IRK"));
    }
    let fields = SensitivePlist(Value::Dictionary(Dictionary::from_iter([
        ("private_key", Value::Data(pairing.private_key_bytes())),
        ("public_key", Value::Data(pairing.public_key_bytes())),
        ("identifier", Value::String(pairing.identifier)),
        ("peer_alt_irk", Value::Data(std::mem::take(&mut *irk))),
    ])));
    atomic::write(path, &fields.seal()?)
}

pub(crate) fn forget_remote_records(data_root: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(data_root.join(DEVICE_DATA_DIR)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if remote_record_serial(&entry.file_name()).is_some() && entry.file_type()?.is_file() {
            match fs::remove_file(entry.path()) {
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                result => result?,
            }
        }
    }
    DeviceInfo::remove(data_root)
}

pub(crate) fn find_paired_device(data_root: &Path) -> Result<PairedDevice, WifiProbeError> {
    let mut paired = paired_devices(data_root)?;
    match paired.len() {
        0 => Err(WifiProbeError::NoPairingRecord),
        1 => Ok(paired.remove(0)),
        _ => Err(WifiProbeError::MultiplePairingRecords),
    }
}

/// Every wireless pairing record, most recently written first.
pub(crate) fn paired_devices(data_root: &Path) -> Result<Vec<PairedDevice>, WifiProbeError> {
    let entries = match fs::read_dir(data_root.join(DEVICE_DATA_DIR)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(WifiProbeError::ReadPairingRecord),
    };
    let mut paired = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| WifiProbeError::ReadPairingRecord)?;
        let name = entry.file_name();
        let serial = remote_record_serial(&name).filter(|id| !id.is_empty());
        if let Some(serial) = serial {
            let metadata = entry
                .metadata()
                .map_err(|_| WifiProbeError::ReadPairingRecord)?;
            if metadata.is_file() {
                let written = metadata.modified().ok();
                let device = PairedDevice {
                    serial: serial.to_owned(),
                    path: entry.path(),
                };
                paired.push((written, device));
            }
        }
    }
    // Newest first; an unreadable time sorts last.
    paired.sort_by(|(left, _), (right, _)| right.cmp(left));
    Ok(paired.into_iter().map(|(_, device)| device).collect())
}

/// Picks one of several pairing records instead of stopping until the user forgets
/// them all. The phone on USB is the one the user is trying to connect, so it wins;
/// otherwise the most recent pairing, which is what the user last set up.
///
/// `records` must be newest first, as [`paired_devices`] returns them. Returns the
/// reason for the diagnostic log.
pub(crate) fn choose_paired_device(
    records: Vec<PairedDevice>,
    usb_serials: &[String],
) -> Option<(PairedDevice, &'static str)> {
    let on_usb = records
        .iter()
        .position(|record| usb_serials.contains(&record.serial));
    let mut records = records;
    match on_usb {
        Some(index) => Some((records.swap_remove(index), "usb")),
        None if records.is_empty() => None,
        None => Some((records.swap_remove(0), "newest")),
    }
}

fn remote_record_serial(name: &OsStr) -> Option<&str> {
    let name = Path::new(name);
    if !name.extension()?.eq_ignore_ascii_case("plist") {
        return None;
    }
    let (prefix, serial) = name
        .file_stem()?
        .to_str()?
        .split_at_checked("remote_".len())?;
    prefix.eq_ignore_ascii_case("remote_").then_some(serial)
}

fn record_key<'a>(fields: &'a Dictionary, key: &str) -> Result<&'a [u8; 32], WifiProbeError> {
    fields
        .get(key)
        .and_then(Value::as_data)
        .and_then(|data| data.try_into().ok())
        .ok_or(WifiProbeError::InvalidPairingRecord)
}

fn record_identifier(
    fields: &Dictionary,
    hostname: impl FnOnce() -> Result<String, WifiProbeError>,
) -> Result<(String, bool), WifiProbeError> {
    let string_field = |key| {
        fields
            .get(key)
            .and_then(Value::as_string)
            .filter(|value| !value.is_empty())
    };
    if let Some(id) = string_field("identifier") {
        return Ok((id.to_owned(), false));
    }
    if let Some(id) = string_field("host_identifier") {
        return Ok((id.to_owned(), true));
    }
    Ok((host_identifier(&hostname()?), true))
}

pub(crate) fn host_identifier(hostname: &str) -> String {
    Uuid::new_v3(&Uuid::NAMESPACE_DNS, hostname.as_bytes())
        .to_string()
        .to_ascii_uppercase()
}

pub(crate) fn windows_hostname() -> Result<String, WifiProbeError> {
    // CPython's socket.gethostname() uses the physical DNS host name on Windows.
    let mut len = 0;
    // SAFETY: A null buffer and zero length request the required capacity.
    unsafe { GetComputerNameExW(ComputerNamePhysicalDnsHostname, ptr::null_mut(), &mut len) };
    if len == 0 {
        return Err(WifiProbeError::HostName);
    }
    let mut buffer = vec![0u16; len as usize];
    // SAFETY: The buffer holds len writable UTF-16 code units and outlives the call.
    let ok = unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            buffer.as_mut_ptr(),
            &mut len,
        )
    };
    if ok == 0 {
        return Err(WifiProbeError::HostName);
    }
    buffer.truncate(len as usize);
    String::from_utf16(&buffer).map_err(|_| WifiProbeError::HostName)
}

struct SensitivePlist(Value);

impl SensitivePlist {
    fn seal(&self) -> io::Result<Vec<u8>> {
        let mut plaintext = Zeroizing::new(Vec::new());
        self.0
            .to_writer_binary(&mut *plaintext)
            .map_err(io::Error::other)?;
        dpapi::protect(&plaintext)
    }
}

impl Drop for SensitivePlist {
    fn drop(&mut self) {
        fn scrub(value: &mut Value) {
            match value {
                Value::Data(bytes) => bytes.zeroize(),
                Value::String(text) => text.zeroize(),
                Value::Array(items) => items.iter_mut().for_each(scrub),
                Value::Dictionary(items) => items.values_mut().for_each(scrub),
                _ => {}
            }
        }
        scrub(&mut self.0);
    }
}

#[cfg(test)]
mod tests;
