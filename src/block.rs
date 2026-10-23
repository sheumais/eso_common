//! Port of EsoMnfBlock.cpp (`CMnfBlock`, `CMnfBlock0`, `CMnfBlock3`).
//!
//! The three C++ classes are one [`MnfBlock`] with a [`BlockKind`]; the virtual methods
//! (`ReadHeader`, `ReadRawData`, `UncompressRawData`, `ParseRawData`, `ReadData`) dispatch on it.
//!
//! Assumptions about the unsupplied `EsoMnfBlock.h` (marked `TODO verify`):
//! * `ZOSFT_BLOCK3_DATACOUNT` is 3 (`MnfFile::create_file_table` uses sections 0..=2),
//!   `ZOSFT_BLOCK0_DATACOUNT` is 1.
//! * `HasData()` is always true for both block types.
//! * The base-class `UncompressRawData` / `ParseRawData` (only overridden by block 3 / block 0 here)
//!   do nothing, so block 0 keeps its section data compressed (`save_block` falls back to it).

use crate::error::Result;
use crate::eso_error;
use crate::file::{BaseFile, CFile};
use crate::util::{guess_file_extension, inflate_zlib_block};
use crate::{print_log};

pub const ZOSFT_BLOCK0_DATACOUNT: usize = 2; // TODO verify against EsoMnfBlock.h
pub const ZOSFT_BLOCK3_DATACOUNT: usize = 3; // TODO verify against EsoMnfBlock.h
pub const ZOSFT_BLOCK_MAXDATACOUNT: usize = 3; // TODO verify against EsoMnfBlock.h
pub const ZOSFT_DATA_OVERSIZE: usize = 32; // TODO verify against EsoMnfBlock.h

/// One data section of a block (`mnf_block_data_t`).
#[derive(Debug, Clone, Default)]
pub struct MnfBlockData {
    /// Size of the section once decompressed, as stored in the file (0 for block 0).
    pub uncompressed_size: u32,
    /// Size of the section as stored in the file.
    pub compressed_size: u32,
    pub compressed_data: Option<Vec<u8>>,
    pub uncompressed_data: Option<Vec<u8>>,
}

impl MnfBlockData {
    /// A section that only holds (already) decompressed bytes.
    pub fn new(data: Vec<u8>) -> Self {
        Self { uncompressed_size: data.len() as u32, uncompressed_data: Some(data), ..Self::default() }
    }
    pub fn destroy(&mut self) {
        *self = Self::default();
    }
    /// The decompressed bytes (`pUncompressedData`).
    pub fn bytes(&self) -> Option<&[u8]> {
        self.uncompressed_data.as_deref()
    }
    pub fn uncompressed_size(&self) -> usize {
        self.uncompressed_data.as_ref().map_or(0, Vec::len)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockKind {
    Block0,
    #[default]
    Block3,
}

/// Union of the `CMnfBlock0` / `CMnfBlock3` header fields.
#[derive(Debug, Clone, Default)]
pub struct MnfBlockHeader {
    pub block_id: u16,
    /// A `word` in block 0, a `dword` in block 3.
    pub unknown1: u32,
    pub record_count1a: u32,
    pub record_count1b: u32,
    /// Number of file table records (`RecordCount23`).
    pub record_count23: u32,
}

#[derive(Debug, Clone, Default)]
pub struct MnfBlock {
    kind: BlockKind,
    little_endian: bool,
    read_block_id: bool,
    header: MnfBlockHeader,
    data: Vec<MnfBlockData>,
}

impl MnfBlock {
    /// A block 3 (`CMnfBlock3(LittleEndian, ReadBlockID)`); kept as the general constructor because
    /// the ZOSFT file table blocks are all of this kind.
    pub fn new(little_endian: bool, read_block_id: bool) -> Self {
        Self::new_block3(little_endian, read_block_id)
    }

    pub fn new_block3(little_endian: bool, read_block_id: bool) -> Self {
        Self {
            kind: BlockKind::Block3,
            little_endian,
            read_block_id,
            header: MnfBlockHeader { block_id: 3, ..Default::default() },
            data: Vec::new(),
        }
    }

    pub fn new_block0(little_endian: bool, read_block_id: bool) -> Self {
        Self {
            kind: BlockKind::Block0,
            little_endian,
            read_block_id,
            header: MnfBlockHeader { block_id: 0, ..Default::default() },
            data: Vec::new(),
        }
    }

    /// `CMnfBlock::CreateBlock`: makes the block matching an on-disk block ID.
    pub fn create_block(block_id: u16) -> Result<Self> {
        match block_id {
            3 => Ok(Self::new_block3(false, false)),
            0 => Ok(Self::new_block0(false, false)),
            _ => Err(eso_error!("Error: Unknown MNF block type {}!", block_id)),
        }
    }

    pub fn kind(&self) -> BlockKind {
        self.kind
    }

    /// Number of data sections the block type contains.
    pub fn data_count(&self) -> usize {
        match self.kind {
            BlockKind::Block0 => ZOSFT_BLOCK0_DATACOUNT,
            BlockKind::Block3 => ZOSFT_BLOCK3_DATACOUNT,
        }
    }

    fn has_data(&self) -> bool {
        true
    }

    pub fn destroy(&mut self) {
        self.header = MnfBlockHeader {
            block_id: match self.kind { BlockKind::Block0 => 0, BlockKind::Block3 => 3 },
            ..Default::default()
        };
        self.data.clear();
    }

    pub fn header(&self) -> &MnfBlockHeader {
        &self.header
    }

    /// `GetData(i)`: the i-th data section (0-based), if present.
    pub fn get_data(&self, i: usize) -> Option<&MnfBlockData> {
        self.data.get(i)
    }

    /// Fills the block directly (used by tests).
    pub fn set_contents(&mut self, header: MnfBlockHeader, data: Vec<MnfBlockData>) {
        self.header = header;
        self.data = data;
    }

    /// `GuessFileExtension(Index)`: empty if the section has no decompressed data.
    pub fn guess_file_extension(&self, index: usize) -> String {
        match self.get_data(index).and_then(MnfBlockData::bytes) {
            Some(b) => guess_file_extension(b).to_string(),
            None => String::new(),
        }
    }

    /// `CMnfBlock::Read(File, ReadBlockID)`.
    pub fn read(&mut self, file: &mut dyn BaseFile, read_block_id: bool) -> Result<()> {
        self.destroy();
        self.read_block_id = read_block_id;

        print_log!("Reading MNF block starting at 0x{:X}...", file.tell());

        self.read_header(file)?;
        self.read_data(file)?;

        print_log!("Finished reading MNF block at 0x{:X}...", file.tell());
        Ok(())
    }

    fn read_header(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        let le = self.little_endian;
        if self.read_block_id {
            self.header.block_id = file.read_u16(le)?;
        }
        match self.kind {
            BlockKind::Block0 => {
                self.header.unknown1 = file.read_u16(le)? as u32;
            }
            BlockKind::Block3 => {
                self.header.unknown1 = file.read_u32(le)?;
                self.header.record_count1a = file.read_u32(le)?;
                self.header.record_count1b = file.read_u32(le)?;
                self.header.record_count23 = file.read_u32(le)?;
            }
        }
        Ok(())
    }

    fn read_data(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        if !self.has_data() {
            print_log!("Skipping empty MNF data block at 0x{:X}...", file.tell());
            return Ok(());
        }

        for i in 0..self.data_count() {
            if i >= ZOSFT_BLOCK_MAXDATACOUNT {
                return Err(eso_error!("Error: Exceeded maximum data count of {} in MNF block!", ZOSFT_BLOCK_MAXDATACOUNT));
            }

            // Block 3 only stores sections 2 and 3 when there are file table records; an empty
            // placeholder keeps the section indices stable.
            if self.kind == BlockKind::Block3 && i >= 1 && self.header.record_count23 == 0 {
                print_log!("Skipping MNF block #{}...", i);
                self.data.push(MnfBlockData::default());
                continue;
            }

            print_log!("Reading MNF block data starting at 0x{:X}...", file.tell());

            let mut d = self.read_raw_data(file)?;
            self.uncompress_raw_data(&mut d)?;
            self.parse_raw_data(&d);
            self.data.push(d);
        }
        Ok(())
    }

    fn read_raw_data(&self, file: &mut dyn BaseFile) -> Result<MnfBlockData> {
        let le = self.little_endian;
        let mut d = MnfBlockData::default();

        if self.kind == BlockKind::Block3 {
            d.uncompressed_size = file.read_u32(le)?;
        }
        d.compressed_size = file.read_u32(le)?;

        if d.compressed_size as usize >= u32::MAX as usize - ZOSFT_DATA_OVERSIZE {
            return Err(eso_error!("Error: Data block in MNF file is too large (0x{:08X} bytes)!", d.compressed_size));
        }
        // Don't allocate gigabytes for a corrupt size field.
        let remaining = file.size().saturating_sub(file.tell());
        if d.compressed_size as u64 > remaining {
            return Err(eso_error!(
                "Error: Data block in MNF file (0x{:08X} bytes) extends past the end of the file!", d.compressed_size));
        }

        let mut buf = vec![0u8; d.compressed_size as usize];
        file.read_bytes(&mut buf)?;
        d.compressed_data = Some(buf);
        Ok(d)
    }

    fn uncompress_raw_data(&self, d: &mut MnfBlockData) -> Result<()> {
        if self.kind != BlockKind::Block3 {
            return Ok(());
        }
        if d.uncompressed_size as usize >= u32::MAX as usize - ZOSFT_DATA_OVERSIZE {
            return Err(eso_error!("Error: Uncompressed data block in MNF file is too large (0x{:08X} bytes)!", d.uncompressed_size));
        }

        let mut out = vec![0u8; d.uncompressed_size as usize + ZOSFT_DATA_OVERSIZE];
        let input = d.compressed_data.as_deref().unwrap_or(&[]);
        let n = inflate_zlib_block(&mut out, input, false)?;

        if n != d.uncompressed_size as usize {
            print_log!("Warning: Actual uncompressed size ({} bytes) doesn't equal expected size ({} bytes)!", n, d.uncompressed_size);
        }
        out.truncate(n);
        d.uncompressed_data = Some(out);
        Ok(())
    }

    fn parse_raw_data(&self, _d: &MnfBlockData) {
        if self.kind == BlockKind::Block0 {
            print_log!("Warning: Don't know how to parse raw data from MNF block0!");
        }
    }

    /// `SaveBlock`: writes the decompressed section, or the raw one if it was never decompressed.
    pub fn save_block(&self, index: usize, filename: &str) -> Result<()> {
        let Some(d) = self.get_data(index) else {
            return Err(eso_error!("Error: Invalid block data index {}!", index));
        };

        let (bytes, what) = if let Some(b) = d.uncompressed_data.as_deref() {
            (b, "uncompressed")
        } else if let Some(b) = d.compressed_data.as_deref() {
            (b, "unknown or compressed")
        } else {
            return Err(eso_error!("Error: No data to export in block {}!", index));
        };

        let mut f = CFile::new();
        f.open(filename, "wb")?;
        print_log!("Saving block #{}, {} bytes of {} data...", index, bytes.len(), what);
        f.write_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::MemoryFile;
    use crate::util::deflate_zlib_block;

    fn be(v: u32) -> [u8; 4] { v.to_be_bytes() }

    fn zsection(raw: &[u8]) -> Vec<u8> {
        let mut c = vec![0u8; raw.len() + 64];
        let n = deflate_zlib_block(&mut c, raw, true).unwrap();
        let mut v = Vec::new();
        v.extend(be(raw.len() as u32));
        v.extend(be(n as u32));
        v.extend(&c[..n]);
        v
    }

    #[test]
    fn reads_block3_with_block_id() {
        let mut f = Vec::new();
        f.extend(3u16.to_be_bytes());
        f.extend(be(9)); f.extend(be(1)); f.extend(be(1)); f.extend(be(2));
        f.extend(zsection(&[1, 2, 3, 4]));
        f.extend(zsection(&[5, 6, 7, 8]));
        f.extend(zsection(&[9; 40]));
        let mut b = MnfBlock::new(false, false);
        b.read(&mut MemoryFile::from_vec(f), true).unwrap();
        assert_eq!(b.header().record_count23, 2);
        assert_eq!(b.get_data(0).unwrap().bytes().unwrap(), &[1, 2, 3, 4]);
        assert_eq!(b.get_data(2).unwrap().bytes().unwrap(), &[9; 40][..]);
    }

    #[test]
    fn block3_without_records_skips_later_sections() {
        let mut f = Vec::new();
        f.extend(be(0)); f.extend(be(0)); f.extend(be(0)); f.extend(be(0));
        f.extend(zsection(&[1, 2]));
        let mut b = MnfBlock::new(false, false);
        b.read(&mut MemoryFile::from_vec(f), false).unwrap();
        assert!(b.get_data(0).unwrap().bytes().is_some());
        assert!(b.get_data(1).unwrap().bytes().is_none());
        assert!(b.get_data(2).unwrap().bytes().is_none());
    }

    #[test]
    fn block0_keeps_raw_data_and_truncated_size_fails() {
        let mut f = Vec::new();
        f.extend(0u16.to_be_bytes());
        f.extend(7u16.to_be_bytes());
        f.extend(be(3)); f.extend([0xAA, 0xBB, 0xCC]);
        f.extend(be(2)); f.extend([0xDD, 0xEE]);
        let mut b = MnfBlock::new_block0(false, false);
        b.read(&mut MemoryFile::from_vec(f), true).unwrap();
        assert_eq!(b.header().unknown1, 7);
        let d0 = b.get_data(0).unwrap();
        assert_eq!(d0.compressed_data.as_deref().unwrap(), &[0xAA, 0xBB, 0xCC]);
        assert!(d0.bytes().is_none());
        let d1 = b.get_data(1).unwrap();
        assert_eq!(d1.compressed_data.as_deref().unwrap(), &[0xDD, 0xEE]);
        assert!(d1.bytes().is_none());

        let mut bad = Vec::new();
        bad.extend(be(0)); bad.extend(be(0)); bad.extend(be(0)); bad.extend(be(1));
        bad.extend(be(4)); bad.extend(be(1_000_000)); // claims far more data than exists
        let mut b = MnfBlock::new(false, false);
        assert!(b.read(&mut MemoryFile::from_vec(bad), false).is_err());
    }

    #[test]
    fn create_block_by_id() {
        assert_eq!(MnfBlock::create_block(0).unwrap().kind(), BlockKind::Block0);
        assert_eq!(MnfBlock::create_block(3).unwrap().kind(), BlockKind::Block3);
        assert!(MnfBlock::create_block(5).is_err());
    }
}
