//! Open records sealed by the Python build's `pair_store.protect` (MAGIC + CryptProtectData).

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData};

const MAGIC: &[u8] = b"ICB-DPAPI1\0";

pub fn unseal(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let Some(sealed) = data.strip_prefix(MAGIC) else {
        return Ok(data.to_vec()); // plaintext record
    };
    let input = CRYPT_INTEGER_BLOB { cbData: sealed.len() as u32, pbData: sealed.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: input points at a live slice; output is allocated by DPAPI and freed below.
    let ok = unsafe {
        CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let out = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData as _) };
    Ok(out)
}
