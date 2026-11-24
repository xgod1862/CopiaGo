//! Display information about the last paired iPhone.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;
use zeroize::Zeroizing;

use crate::atomic;
use crate::dpapi;
use crate::paths::{self, DEVICE_DATA_DIR};

const INFO_FILE: &str = "device-info.bin";

/// Empty fields indicate details that have not been reported by the phone yet.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceInfo {
    pub name: String,
    pub model: String,
    pub os_version: String,
}

impl DeviceInfo {
    /// Reads the last paired phone's details. Missing or unreadable info yields empty fields.
    pub fn load(data_dir: &Path) -> Self {
        Self::read(data_dir).unwrap_or_default()
    }

    /// Stores the display information in the Python-compatible, DPAPI-sealed file.
    pub fn save(&self, data_dir: &Path) -> io::Result<()> {
        paths::ensure_data_dir(data_dir)?;
        let directory = data_dir.join(DEVICE_DATA_DIR);
        paths::ensure_data_dir(&directory)?;
        let values = BTreeMap::from([
            ("name", &self.name),
            ("model", &self.model),
            ("os_version", &self.os_version),
        ]);
        let plaintext = Zeroizing::new(serde_json::to_vec(&values)?);
        let sealed = dpapi::protect(&plaintext)?;
        atomic::write(&directory.join(INFO_FILE), &sealed)
    }

    /// Removes the cached details, including when no details were saved.
    pub fn remove(data_dir: &Path) -> io::Result<()> {
        match fs::remove_file(data_dir.join(DEVICE_DATA_DIR).join(INFO_FILE)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    fn read(data_dir: &Path) -> io::Result<Self> {
        let file = Zeroizing::new(fs::read(data_dir.join(DEVICE_DATA_DIR).join(INFO_FILE))?);
        let plaintext = Zeroizing::new(dpapi::unprotect(&file)?);
        let values: Value = serde_json::from_slice(&plaintext)?;
        Ok(Self {
            name: display_string(&values["name"]),
            model: display_string(&values["model"]),
            os_version: display_string(&values["os_version"]),
        })
    }
}

fn display_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Number(number) if number.as_f64() != Some(0.0) => number.to_string(),
        _ => String::new(),
    }
}
