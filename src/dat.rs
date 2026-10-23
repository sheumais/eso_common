//! Port of EsoDatFile.cpp: reading and decompressing DAT sub-files.
//!
//! The C++ structs used raw pointers into the uncompressed buffer; here those become byte ranges into
//! `uncompressed_data`.

use crate::error::Result;
use crate::file::{BaseFile, CFile};
use crate::mnf::MnfHeader;
use crate::util::{inflate_snappy_block, inflate_zlib_block, is_valid_snappy};
use crate::{eso_debug_error, print_log};
use std::ops::Range;

#[derive(Debug, Clone, Default)]
pub struct DatSubFileInfo {
    pub filename: String,
    pub offset: u64,
    pub raw_size: u32,
    pub uncompressed_size: u32,
    /// 0 = none, 1 = zlib, 2 = snappy
    pub compress_type: u32,
    pub archive_index: u8,

    pub raw_data: Option<Vec<u8>>,
    pub uncompressed_data: Option<Vec<u8>>,

    pub null_byte: u32,
    pub data_size1: u32,
    pub data_size2: u32,
    /// Ranges into `uncompressed_data`.
    pub data1: Range<usize>,
    pub data2: Range<usize>,
    /// Range into `uncompressed_data` of the Oodle compressed payload (v3 files), if it was decompressed.
    pub oodle_compressed: Range<usize>,
    /// Oodle decompressed data (v3 files); when set, `file_data` indexes into this instead.
    pub oodle_decompressed: Option<Vec<u8>>,
    /// Range of the actual file contents (into `oodle_decompressed` if set, else `uncompressed_data`).
    pub file_data: Range<usize>,
}

impl DatSubFileInfo {
    /// The "real" file contents (`pFileDataStart` / `FileDataSize` in the C++).
    pub fn file_bytes(&self) -> &[u8] {
        let buf = self.oodle_decompressed.as_deref().or(self.uncompressed_data.as_deref());
        buf.and_then(|b| b.get(self.file_data.clone())).unwrap_or(&[])
    }

    /// True once uncompressed data exists (`pFileDataStart != nullptr`).
    pub fn has_file_data(&self) -> bool {
        self.uncompressed_data.is_some() || self.oodle_decompressed.is_some()
    }
}

pub fn parse_sub_file_raw_data(info: &mut DatSubFileInfo, header: &MnfHeader) -> Result<()> {
    // Temporarily take the buffer so `info` can be updated while the data is borrowed.
    let Some(data) = info.uncompressed_data.take() else {
        return Err(eso_debug_error!("Error: No uncompressed data in DAT sub-file to parse!"));
    };
    let result = parse_impl(info, &data, header);
    info.uncompressed_data = Some(data);
    result
}

fn parse_impl(info: &mut DatSubFileInfo, data: &[u8], header: &MnfHeader) -> Result<()> {
    let size = (info.uncompressed_size as usize).min(data.len());
    if size < 4 {
        return Err(eso_debug_error!("Error: Uncompressed data in DAT sub-file too small to parse!"));
    }

    if header.type_ != 6 {
        info.data_size1 = 0;
        info.data_size2 = 0;
        info.data1 = 0..0;
        info.data2 = 0..0;
        info.file_data = 0..size;
        return Ok(());
    }

    info.null_byte = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if info.null_byte != 0 { print_log!("\tNullByte = 0x{:08X}", info.null_byte); }

    if size < 8 { return Err(eso_debug_error!("Error: Missing data1 size in DAT sub-file!")); }
    info.data_size1 = u32::from_be_bytes(data[4..8].try_into().unwrap());
    let s1 = info.data_size1 as usize;

    if size < 8 + s1 { return Err(eso_debug_error!("Error: Missing data1 record data in DAT sub-file!")); }
    info.data1 = 8..8 + s1;

    if size < 12 + s1 { return Err(eso_debug_error!("Error: Missing data2 size in DAT sub-file!")); }
    info.data_size2 = u32::from_be_bytes(data[s1 + 8..s1 + 12].try_into().unwrap());
    let s2 = info.data_size2 as usize;

    if size < 12 + s1 + s2 { return Err(eso_debug_error!("Error: Missing data2 record data in DAT sub-file!")); }
    info.data2 = 12 + s1..12 + s1 + s2;
    info.file_data = 12 + s1 + s2..size;

    if is_valid_snappy(&data[info.data1.clone()]) { print_log!("\tData1 is snappy format"); }
    if is_valid_snappy(&data[info.data2.clone()]) { print_log!("\tData2 is snappy format"); }

    let mut temp = [0u8; 2048];
    if inflate_zlib_block(&mut temp[..2040], &data[info.data1.clone()], true).is_ok() { print_log!("\tData1 is zlib format"); }
    if inflate_zlib_block(&mut temp[..2040], &data[info.data2.clone()], true).is_ok() { print_log!("\tData2 is zlib format"); }

    if info.data_size1 < 0x1B8 || info.data_size1 > 0x1BC { print_log!("\tDataSize1 = 0x{:08X}", info.data_size1); }
    if info.data_size2 != 0x28 { print_log!("\tDataSize2 = 0x{:08X}", info.data_size2); }

    Ok(())
}

/// Reads the raw (still compressed) sub-file bytes.  Opens `info.filename` if `input` is `None`.
pub fn read_sub_file_raw_data(info: &mut DatSubFileInfo, _header: &MnfHeader, input: Option<&mut CFile>) -> Result<()> {
    if info.raw_size > i32::MAX as u32 {
        return Err(eso_debug_error!("Error: Input size of DAT sub-file is too large ({} bytes)!", info.raw_size));
    }

    let mut local = CFile::new();
    let file = match input {
        Some(f) => f,
        None => {
            local.open(&info.filename, "rb")?;
            &mut local
        }
    };

    file.seek_to(info.offset)?;
    let mut buf = vec![0u8; info.raw_size as usize];
    file.read_bytes(&mut buf)?;
    info.raw_data = Some(buf);
    Ok(())
}

/// Reads and decompresses a sub-file, then splits it into its data1/data2/file-data parts.
pub fn read_sub_file_data(info: &mut DatSubFileInfo, header: &MnfHeader, input: Option<&mut CFile>) -> Result<()> {
    read_sub_file_raw_data(info, header, input)
        .map_err(|_| eso_debug_error!("\tError: Failed to read raw data for DAT subfile!"))?;

    if info.uncompressed_size > i32::MAX as u32 {
        return Err(eso_debug_error!("\tError: Uncompressed size of DAT sub-file is too large ({} bytes)!", info.uncompressed_size));
    }

    match info.compress_type {
        0 => {
            info.uncompressed_data = info.raw_data.take();
            info.uncompressed_size = info.raw_size;
        }
        1 => {
            let mut out = vec![0u8; info.uncompressed_size as usize + 100];
            let raw = info.raw_data.as_deref().unwrap_or(&[]);
            inflate_zlib_block(&mut out[..info.uncompressed_size as usize], raw, false)?;
            info.uncompressed_data = Some(out);
        }
        2 => {
            let mut out = vec![0u8; info.uncompressed_size as usize + 100];
            let raw = info.raw_data.as_deref().unwrap_or(&[]);
            if let Err(e) = inflate_snappy_block(&mut out[..info.uncompressed_size as usize], raw, false) {
                crate::print_debug!("Error: Failed to decompress the Snappy data!");
                return Err(e);
            }
            info.uncompressed_data = Some(out);
        }
        other => return Err(eso_debug_error!("Error: Unknown compression type {} found in DAT sub-file!", other)),
    }

    parse_sub_file_raw_data(info, header)
}
