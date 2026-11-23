use std::{env, io, path::PathBuf};

use copiago_core::paths::{self, RunMode};
use copiago_win::instance::Instance;

pub(super) struct Launch {
    pub instance: Instance,
    pub isolated: bool,
    pub activate: bool,
}

impl Launch {
    pub fn acquire(minimized: bool) -> io::Result<Option<Self>> {
        let namespace = env::var_os("ICB_INSTANCE_NAMESPACE");
        let isolated = namespace.is_some();
        let instance = if let Some(namespace) = namespace {
            if env::var_os("ICB_DATA_DIR").is_none_or(|path| path.is_empty()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "ICB_INSTANCE_NAMESPACE requires a separate ICB_DATA_DIR",
                ));
            }
            let namespace = namespace
                .into_string()
                .map_err(|_| io::ErrorKind::InvalidInput)?;
            Instance::acquire_isolated(&namespace, minimized)?
        } else {
            Instance::acquire(minimized)?
        };
        Ok(instance.map(|instance| Self {
            instance,
            isolated,
            activate: !(isolated
                && env::var_os("ICB_NO_ACTIVATE").as_deref() == Some("1".as_ref())),
        }))
    }

    pub fn data_dir(&self, headless: bool) -> io::Result<PathBuf> {
        let portable = env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("portable-data")))
            .is_some_and(|marker| marker.is_file());
        let mode = if portable || (!headless && !cfg!(debug_assertions)) {
            RunMode::Packaged
        } else {
            RunMode::Development
        };
        paths::data_dir(mode)
    }
}
