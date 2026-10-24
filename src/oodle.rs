//! Runtime loader for the Oodle decompression library (`LoadOodleLib` / oodle.h were not supplied).
//!
//! The Oodle DLL (e.g. `oo2core_8_win64.dll`, shipped with ESO and many other games) is loaded
//! dynamically.  The exported function is `OodleLZ_Decompress`; the signature below is the public
//! Oodle 2.x one, but it has not been exercised against a real library yet.

use crate::error::{EsoError, Result};
use crate::{eso_error, print_error, print_log};
use libloading::Library;
use std::ffi::c_void;
use std::path::Path;
use std::sync::OnceLock;

type OodleLzDecompress = unsafe extern "C" fn(
    comp_buf: *const c_void, comp_buf_size: isize,
    raw_buf: *mut c_void, raw_len: isize,
    fuzz_safe: i32, check_crc: i32, verbosity: i32,
    dec_buf_base: *mut c_void, dec_buf_size: isize,
    fp_callback: *mut c_void, callback_user_data: *mut c_void,
    decoder_memory: *mut c_void, decoder_memory_size: isize,
    thread_phase: i32,
) -> isize;

struct Oodle {
    _lib: Library,
    decompress: OodleLzDecompress,
}

static OODLE: OnceLock<Oodle> = OnceLock::new();

const CANDIDATES: &[&str] = &[
    "oo2core_8_win64.dll", "oo2core_9_win64.dll", "oo2core_7_win64.dll", "oo2core_6_win64.dll",
    "oo2core_5_win64.dll", "liboo2corelinux64.so.9", "liboo2corelinux64.so",
];

/// The library names to try, in order: `path` first (if given), then the usual names.
///
/// If `path` is a directory, each candidate file name is tried inside it, so the game's tool
/// folder can be passed directly.
fn candidate_names(path: Option<&str>) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(p) = path {
        if Path::new(p).is_dir() {
            names.extend(CANDIDATES.iter().map(|c| Path::new(p).join(c).to_string_lossy().into_owned()));
        } else {
            names.push(p.to_string());
        }
    }
    names.extend(CANDIDATES.iter().map(|c| c.to_string()));
    names
}

/// Tries to load the Oodle library. `path` may name the library file or a folder containing it;
/// the usual names are tried afterwards. Returns `false` if no library could be loaded.
pub fn load_oodle_lib_from(path: Option<&str>) -> bool {
    if OODLE.get().is_some() { return true; }

    for name in candidate_names(path) {
        // SAFETY: loading a shared library runs its initialisers; we only load Oodle.
        let Ok(lib) = (unsafe { Library::new(&name) }) else { continue };
        // SAFETY: symbol type matches the documented Oodle 2.x C signature.
        let sym = unsafe { lib.get::<OodleLzDecompress>(b"OodleLZ_Decompress\0") };
        if let Ok(sym) = sym {
            let decompress = *sym;
            print_log!("Loaded Oodle library '{}'", name);
            let _ = OODLE.set(Oodle { _lib: lib, decompress });
            return true;
        }
    }
    print_error!("Error: Failed to load the Oodle library (looked for {}).", CANDIDATES.join(", "));
    false
}

/// Like [`load_oodle_lib_from`], but reports failure as an error value.
pub fn try_load_oodle_lib_from(path: Option<&str>) -> Result<()> {
    if load_oodle_lib_from(path) {
        Ok(())
    } else {
        Err(EsoError::new("Oodle library not found; Oodle-compressed entries cannot be read"))
    }
}

pub fn load_oodle_lib() -> bool {
    load_oodle_lib_from(None)
}

pub fn is_oodle_loaded() -> bool {
    OODLE.get().is_some()
}

/// Decompresses an Oodle block into `output`; `raw_len` is the expected uncompressed size
/// (`output` may be larger).  Arguments mirror the C++ call in `ParseDataFileVer3`.
pub fn oodle_decompress(input: &[u8], output: &mut [u8], raw_len: usize) -> Result<usize> {
    let Some(o) = OODLE.get() else { return Err(eso_error!("Error: The Oodle library has not been loaded!")) };
    // SAFETY: pointers/lengths come from valid slices; remaining args are the Oodle defaults.
    let n = unsafe {
        (o.decompress)(
            input.as_ptr().cast(), input.len() as isize,
            output.as_mut_ptr().cast(), raw_len.min(output.len()) as isize,
            0, 0, 0,
            std::ptr::null_mut(), 0, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0,
            3,
        )
    };
    if n <= 0 { return Err(eso_error!("Error: Oodle failed to decompress the data!")); }
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_is_expanded_to_candidate_files() {
        let dir = std::env::temp_dir().join(format!("eso_oodle_dir_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = candidate_names(Some(dir.to_str().unwrap()));
        assert_eq!(names.len(), CANDIDATES.len() * 2, "folder candidates, then the bare names");
        assert_eq!(Path::new(&names[0]), dir.join(CANDIDATES[0]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_file_path_is_tried_first() {
        let names = candidate_names(Some("/opt/game/custom_oodle.so"));
        assert_eq!(names[0], "/opt/game/custom_oodle.so");
        assert_eq!(names.len(), CANDIDATES.len() + 1);
    }
}
