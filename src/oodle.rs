//! Runtime loader for the Oodle decompression library (`LoadOodleLib` / oodle.h were not supplied).
//!
//! The Oodle DLL (e.g. `oo2core_8_win64.dll`, shipped with ESO and many other games) is loaded
//! dynamically.  The exported function is `OodleLZ_Decompress`; the signature below is the public
//! Oodle 2.x one, but it has not been exercised against a real library yet.

use crate::error::Result;
use crate::{eso_error, print_error, print_log};
use libloading::Library;
use std::ffi::c_void;
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

/// Tries to load the Oodle library (first from `path` if given, then from the usual names).
pub fn load_oodle_lib_from(path: Option<&str>) -> bool {
    if OODLE.get().is_some() { return true; }

    let names: Vec<&str> = path.into_iter().chain(CANDIDATES.iter().copied()).collect();
    for name in names {
        // SAFETY: loading a shared library runs its initialisers; we only load Oodle.
        let Ok(lib) = (unsafe { Library::new(name) }) else { continue };
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
