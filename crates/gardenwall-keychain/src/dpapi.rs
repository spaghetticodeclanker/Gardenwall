//! Windows DPAPI wrapping (current-user scope, no UI).

#![allow(unsafe_code)]

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use zeroize::Zeroizing;

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
    }
}

/// Copy an API-allocated blob out, zero it, and free it.
fn take(out: CRYPT_INTEGER_BLOB) -> Zeroizing<Vec<u8>> {
    // SAFETY: on success the API returns `cbData` initialized bytes at
    // `pbData`, allocated with LocalAlloc.
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) };
    let copy = Zeroizing::new(bytes.to_vec());
    // SAFETY: the buffer is ours to modify and free; zero it first so the
    // key does not linger in freed heap memory.
    unsafe {
        std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
        LocalFree(out.pbData.cast());
    }
    copy
}

pub fn protect(plain: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String> {
    let input = blob(plain);
    let extra = blob(entropy);
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    // SAFETY: input and entropy point to live slices for the call; `out`
    // receives a LocalAlloc'd buffer released by `take`.
    let ok = unsafe {
        CryptProtectData(
            &input,
            null(),
            &extra,
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        // SAFETY: trivially safe.
        return Err(format!("CryptProtectData failed (error {})", unsafe {
            GetLastError()
        }));
    }
    Ok(take(out).to_vec())
}

pub fn unprotect(wrapped: &[u8], entropy: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    let input = blob(wrapped);
    let extra = blob(entropy);
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    // SAFETY: as in `protect`.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            null_mut(),
            &extra,
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        // SAFETY: trivially safe.
        return Err(format!("CryptUnprotectData failed (error {})", unsafe {
            GetLastError()
        }));
    }
    Ok(take(out))
}
