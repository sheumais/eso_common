//! RIFF -> WAV conversion (`ConvertRiffFile` / `DoConvertExistingRiffFiles` from EsoExtractData.cpp).
//! Lives in the common crate because the MNF exporter uses it for the default RIFF conversion.

use crate::error::Result;
use crate::file::CFile;
use crate::util::{remove_file_extension, string_ends_with};
use crate::{eso_error, print_error, print_log};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

pub static RIFF_FILE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static RIFF_ERROR_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Converts the ESO ".riff" file to a standard ".wav" next to it: 4 bytes are inserted at offset 72,
/// the size fields are patched and the WAVEFORMATEXTENSIBLE sub-format GUID is written.
pub fn convert_riff_file(filename: &str) -> Result<()> {
    print_error!("Converting RIFF file {}...", filename);

    let src = CFile::read_all_from(filename)?;
    if src.len() > i32::MAX as usize {
        return Err(eso_error!("RIFF filesize of {} bytes is too large to convert!", src.len()));
    }
    if src.len() < 72 {
        return Err(eso_error!("RIFF filesize of {} bytes is too small to convert!", src.len()));
    }

    let new_size = src.len() + 4;
    let mut out = vec![0u8; new_size];
    out[..72].copy_from_slice(&src[..72]);
    out[76..].copy_from_slice(&src[72..]);

    let riff_size = u32::from_le_bytes(src[4..8].try_into().unwrap()).wrapping_add(4);
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());

    let fmt_size = u32::from_le_bytes(src[16..20].try_into().unwrap());
    if fmt_size != 24 { print_log!("\tFMT size of {}", fmt_size); }
    out[16..20].copy_from_slice(&fmt_size.wrapping_add(0x10).to_le_bytes());

    let cb_size = u16::from_le_bytes(src[36..38].try_into().unwrap());
    if cb_size != 6 { print_log!("\tCB size of {}", cb_size); }
    out[36..38].copy_from_slice(&cb_size.wrapping_add(0x10).to_le_bytes());

    let valid_bits = u16::from_le_bytes(src[38..40].try_into().unwrap());
    if valid_bits != 0 { print_log!("\tValidBitsPerSample of {}", valid_bits); }
    out[38..40].copy_from_slice(&valid_bits.wrapping_add(0x10).to_le_bytes());

    out[44..60].copy_from_slice(b"\x01\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xAA\x00\x38\x9B\x71");

    let out_name = format!("{}.wav", remove_file_extension(filename));
    let mut f = CFile::new();
    f.open(&out_name, "wb")?;
    f.write_bytes(&out)
}

/// Recursively converts every `.riff` under `root_path` (which should end with a path separator).
pub fn convert_existing_riff_files(root_path: &str) -> Result<()> {
    let entries = fs::read_dir(root_path)
        .map_err(|_| eso_error!("Error: Failed to find any RIFF files to convert!"))?;

    print_error!("Converting RIFF files in {}...", root_path);

    let mut sub_dirs = Vec::new();
    for entry in entries.flatten() {
        RIFF_FILE_COUNT.fetch_add(1, Ordering::Relaxed);
        let path = entry.path();
        if path.is_dir() {
            sub_dirs.push(format!("{}{}", crate::util::terminate_path(&path.to_string_lossy()), ""));
        } else {
            let name = path.to_string_lossy().into_owned();
            if string_ends_with(&name, ".riff") && convert_riff_file(&name).is_err() {
                RIFF_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    for dir in sub_dirs {
        let _ = convert_existing_riff_files(&dir);
    }
    Ok(())
}
