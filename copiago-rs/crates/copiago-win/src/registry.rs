use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, WIN32_ERROR},
    System::Registry::{
        HKEY, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW,
        RegOpenKeyExW, RegSetValueExW,
    },
};

pub(crate) fn status(code: WIN32_ERROR) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

pub(crate) struct Key(pub(crate) HKEY);

impl Key {
    pub(crate) fn open(parent: HKEY, path: &str, access: REG_SAM_FLAGS) -> io::Result<Self> {
        let mut key = null_mut();
        let path: Vec<_> = path.encode_utf16().chain([0]).collect();
        // SAFETY: The parent is valid, path is NUL-terminated, and key is writable.
        status(unsafe { RegOpenKeyExW(parent, path.as_ptr(), 0, access, &mut key) })?;
        Ok(Self(key))
    }

    pub(crate) fn create(parent: HKEY, path: &str, access: REG_SAM_FLAGS) -> io::Result<Self> {
        let mut key = null_mut();
        let path: Vec<_> = path.encode_utf16().chain([0]).collect();
        // SAFETY: The parent is valid, path is NUL-terminated, and key is writable.
        status(unsafe {
            RegCreateKeyExW(
                parent,
                path.as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                access,
                null(),
                &mut key,
                null_mut(),
            )
        })?;
        Ok(Self(key))
    }

    pub(crate) fn write(
        &self,
        name: &str,
        value_type: REG_VALUE_TYPE,
        value: &[u8],
    ) -> io::Result<()> {
        let size = u32::try_from(value.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
        let name: Vec<_> = name.encode_utf16().chain([0]).collect();
        // SAFETY: name is NUL-terminated, and value supplies size initialized bytes.
        status(unsafe {
            RegSetValueExW(self.0, name.as_ptr(), 0, value_type, value.as_ptr(), size)
        })
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: This key owns one handle from the registry API.
        unsafe { RegCloseKey(self.0) };
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, KEY_ALL_ACCESS, RegDeleteTreeW};

    use super::{HKEY, Key};

    pub(crate) struct Fixture {
        key: Option<Key>,
        path: Vec<u16>,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let pid = std::process::id();
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let index = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = format!("Software\\CopiaGo.T-WIN-02.Test-{pid}-{now}-{index}");
            let key = Key::create(HKEY_CURRENT_USER, &path, KEY_ALL_ACCESS).unwrap();
            Self {
                key: Some(key),
                path: path.encode_utf16().chain([0]).collect(),
            }
        }

        pub(crate) fn handle(&self) -> HKEY {
            self.key.as_ref().map_or(null_mut(), |key| key.0)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.key.take();
            // SAFETY: path is the unique NUL-terminated test key created by new().
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, self.path.as_ptr()) };
        }
    }
}
