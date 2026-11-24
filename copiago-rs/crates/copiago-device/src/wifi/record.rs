use std::fs;
use std::io::{Cursor, ErrorKind};
use std::path::{Path, PathBuf};
use std::ptr;

use copiago_core::{atomic, dpapi, paths::DEVICE_DATA_DIR};
use ed25519_dalek::{SigningKey, VerifyingKey};
use idevice::remote_pairing::RpPairingFile;
use plist::{Dictionary, Value};
use uuid::Uuid;
use windows_sys::Win32::System::SystemInformation::{
    ComputerNamePhysicalDnsHostname, GetComputerNameExW,
};
use zeroize::{Zeroize, Zeroizing};

use super::{IdentifierMigration, WifiProbeError};

pub(super) struct RemoteRecord {
    pub(super) pairing: RpPairingFile,
    path: PathBuf,
    original: Zeroizing<Vec<u8>>,
    fields: SensitivePlist,
    needs_identifier: bool,
}

impl RemoteRecord {
    pub(super) fn read(data_root: &Path) -> Result<Self, WifiProbeError> {
        let path = record_path(data_root)?;
        let original =
            Zeroizing::new(fs::read(&path).map_err(|_| WifiProbeError::ReadPairingRecord)?);
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
            path,
            original,
            fields,
            needs_identifier,
        })
    }

    pub(super) fn peer_alt_irk(&self) -> Option<&[u8; 16]> {
        let fields = self.fields.0.as_dictionary()?;
        ["peer_alt_irk", "alt_irk"].into_iter().find_map(|key| {
            fields
                .get(key)?
                .as_data()
                .and_then(|data| data.try_into().ok())
        })
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
        let mut plaintext = Zeroizing::new(Vec::new());
        self.fields
            .0
            .to_writer_binary(&mut *plaintext)
            .map_err(|_| WifiProbeError::InvalidPairingRecord)?;
        let sealed = dpapi::protect(&plaintext).map_err(|_| WifiProbeError::ReadPairingRecord)?;

        // Verification can take several seconds. Do not replace a record that was
        // refreshed or removed while the device was being contacted.
        let current =
            Zeroizing::new(fs::read(&self.path).map_err(|_| WifiProbeError::ReadPairingRecord)?);
        if *current != *self.original {
            return Err(WifiProbeError::ReadPairingRecord);
        }
        atomic::write(&self.path, &sealed).map_err(|_| WifiProbeError::ReadPairingRecord)
    }
}

fn record_path(data_root: &Path) -> Result<PathBuf, WifiProbeError> {
    let entries = fs::read_dir(data_root.join(DEVICE_DATA_DIR)).map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            WifiProbeError::NoPairingRecord
        } else {
            WifiProbeError::ReadPairingRecord
        }
    })?;
    let mut path = None;
    for entry in entries {
        let entry = entry.map_err(|_| WifiProbeError::ReadPairingRecord)?;
        let name = entry.file_name();
        let is_record = name
            .to_str()
            .and_then(|name| name.strip_prefix("remote_"))
            .and_then(|name| name.strip_suffix(".plist"))
            .is_some_and(|id| !id.is_empty());
        if is_record && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            if path.is_some() {
                return Err(WifiProbeError::MultiplePairingRecords);
            }
            path = Some(entry.path());
        }
    }
    path.ok_or(WifiProbeError::NoPairingRecord)
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
    let host = hostname()?;
    let id = Uuid::new_v3(&Uuid::NAMESPACE_DNS, host.as_bytes());
    Ok((id.to_string().to_ascii_uppercase(), true))
}

fn windows_hostname() -> Result<String, WifiProbeError> {
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
