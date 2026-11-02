//! How imported cookies are kept (`reelgene_core::cookies`): on Windows
//! sealed with DPAPI, which only the same user on the same computer can
//! open; elsewhere in a file only the user can read, as browsers keep theirs.

#[cfg(not(windows))]
use reelgene_core::cookies::Plain;
use reelgene_core::cookies::{CookieJar, Seal};
use std::path::Path;

/// The jar in `<data>/cookies.sealed`, sealed the system's way.
pub fn cookie_jar(data_dir: &Path) -> CookieJar {
    CookieJar::new(data_dir.join("cookies.sealed"), system())
}

pub fn system() -> Box<dyn Seal> {
    #[cfg(windows)]
    {
        Box::new(Dpapi)
    }
    #[cfg(not(windows))]
    {
        Box::new(Plain)
    }
}

#[cfg(windows)]
struct Dpapi;

#[cfg(windows)]
impl Seal for Dpapi {
    fn name(&self) -> &'static str {
        "dpapi"
    }

    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>, String> {
        dpapi::run(plain, true)
    }

    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, String> {
        dpapi::run(sealed, false)
    }
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    /// Seal (`protect`) or open `data` for the current user; never shows UI.
    pub fn run(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
        let len = u32::try_from(data.len()).map_err(|_| "too large".to_owned())?;
        let input = CRYPT_INTEGER_BLOB {
            cbData: len,
            // Read only: DPAPI takes a `*mut` for an input it does not write.
            pbData: data.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: `input` points at `len` live bytes for the whole call; the
        // optional pointers are null, which DPAPI allows; `output` is written
        // by the call and freed below with LocalFree, as DPAPI requires.
        #[allow(unsafe_code)]
        let ok = unsafe {
            if protect {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            }
        };
        if ok == 0 || output.pbData.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: on success `output` holds `cbData` bytes allocated by DPAPI,
        // copied out before they are freed exactly once.
        #[allow(unsafe_code)]
        let bytes = unsafe {
            let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(output.pbData.cast());
            bytes
        };
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_systems_seal_opens_what_it_sealed_and_hides_it() {
        let seal = system();
        let text = b".site.example\tTRUE\t/\tTRUE\t0\tSID\tsecret\n";
        let sealed = seal.seal(text).unwrap();
        assert_eq!(seal.open(&sealed).unwrap(), text);
        if cfg!(windows) {
            assert_eq!(seal.name(), "dpapi");
            assert!(!sealed.windows(6).any(|w| w == b"secret"));
            assert!(seal.open(b"not sealed").is_err());
        } else {
            assert_eq!(seal.name(), "plain");
        }
    }
}
