//! Windows current-user DPAPI formats shared with the Python version.

use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows_sys::core::PCWSTR;
use windows_sys::w;
use zeroize::Zeroize;

use crate::win32::LocalAllocation;

pub const MAGIC: &[u8] = b"ICB-DPAPI1\0";

/// Seals a pairing record or device info with the CopiaGo header.
pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    crypt(data, Some(w!("CopiaGo pairing record")), MAGIC)
}

/// Opens a sealed record or returns the bytes of an older plaintext record.
pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    match data.strip_prefix(MAGIC) {
        Some(sealed) => crypt(sealed, None, &[]),
        None => Ok(data.to_vec()),
    }
}

/// Seals a history cell in the raw DPAPI format used by SQLite preview and payload columns.
pub fn protect_history(data: &[u8]) -> io::Result<Vec<u8>> {
    crypt(data, Some(w!("CopiaGo")), &[])
}

/// Opens a history cell in raw DPAPI format.
pub fn unprotect_history(data: &[u8]) -> io::Result<Vec<u8>> {
    crypt(data, None, &[])
}

fn crypt(data: &[u8], description: Option<PCWSTR>, prefix: &[u8]) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "DPAPI input is too large"))?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };

    // SAFETY: The input and its byte slice stay live and are read-only. Descriptions
    // are static NUL-terminated UTF-16 strings. DPAPI initializes output on success.
    let succeeded = unsafe {
        match description {
            Some(description) => CryptProtectData(
                &input,
                description,
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            ),
            None => CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            ),
        }
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: On success the DPAPI output buffer is owned and freed using LocalFree.
    let allocation = unsafe { LocalAllocation::from_raw(output.pbData) };
    let len = output.cbData as usize;
    let mut result = Vec::with_capacity(prefix.len() + len);
    result.extend_from_slice(prefix);
    if len != 0 {
        let allocation = allocation
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing DPAPI output"))?;
        // SAFETY: A successful DPAPI call initialized len bytes in the owned buffer.
        let bytes = unsafe { std::slice::from_raw_parts_mut(allocation.as_ptr(), len) };
        result.extend_from_slice(bytes);
        bytes.zeroize();
    }
    Ok(result)
}
