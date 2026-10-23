//! Port of EsoZosftFile.cpp (`CZosftFile`): the ZOS file table that maps MNF file indices to filenames.
//!
//! Assumptions about the unsupplied EsoZosftFile.h: the magic is `"ZOSFT"` for both header and footer
//! (5 bytes), there are 3 blocks (`ZOSFT_BLOCK_COUNT`, `TestCount80(2, 0)` is the highest index
//! used), and `GetSize()` is the number of file table entries.

use crate::block::MnfBlock;
use crate::error::Result;
use crate::file::{BaseFile, CFile};
use crate::util::{ensure_path_exists, extract_path, get_timer_ms, is_valid_snappy};
use crate::{cprintf, print_error, print_log};
use std::collections::HashMap;

pub const ZOSFT_MAGIC_ID: &[u8] = b"ZOSFT";
pub const ZOSFT_HEADER_MAGICSIZE: usize = 5; // TODO verify against EsoZosftFile.h
pub const ZOSFT_FOOTER_MAGICSIZE: usize = 5; // TODO verify against EsoZosftFile.h
pub const ZOSFT_BLOCK_COUNT: usize = 3; // TODO verify against EsoZosftFile.h

#[derive(Debug, Clone, Default)]
pub struct ZosftHeader {
    pub file_id: [u8; ZOSFT_HEADER_MAGICSIZE],
    pub unknown1: u16,
    pub unknown2: u32,
    pub unknown3: u32,
    pub record_count: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ZosftFileEntry {
    pub index: u32,
    pub index11: u32,
    pub index13: u32,
    pub index21: u32,
    pub file_index: u32,
    pub filename_offset: u32,
    pub file_id: u64,
    /// Number of MNF entries that link to this entry.
    pub user_data: u32,
    /// Filename with `/` replaced by `\`.
    pub filename: String,
}

#[derive(Debug, Default)]
pub struct ZosftFile {
    header: ZosftHeader,
    footer: [u8; ZOSFT_FOOTER_MAGICSIZE],
    raw_file_data: Vec<u8>,
    /// Raw NUL-separated names *including* the trailing NUL (as the C++ stored them).
    filenames: Vec<String>,
    blocks: [MnfBlock; ZOSFT_BLOCK_COUNT],
    pub file_table: Vec<ZosftFileEntry>,
    file_index_map: HashMap<u32, usize>,
    mnf_version: u32,
}

fn rd_u32(d: &[u8], off: usize) -> u32 {
    d.get(off..off + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn rd_u64(d: &[u8], off: usize) -> u64 {
    d.get(off..off + 8).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
}

/// Advances past the current 4-byte record to the next one whose high byte is 0x80
/// (`do { off += 4 } while (off < size && data[off+3] != 0x80)`).
fn skip_to_next_marker(d: &[u8], mut off: usize) -> usize {
    loop {
        off += 4;
        if !(off < d.len() && d.get(off + 3).map_or(false, |&b| b != 0x80)) {
            return off;
        }
    }
}

impl ZosftFile {
    pub fn new(mnf_version: u32) -> Self {
        Self { mnf_version, ..Self::default() }
    }

    pub fn set_version(&mut self, v: u32) { self.mnf_version = v; }
    pub fn version(&self) -> u32 { self.mnf_version }
    pub fn header(&self) -> &ZosftHeader { &self.header }
    pub fn filenames(&self) -> &[String] { &self.filenames }
    pub fn get_size(&self) -> usize { self.file_table.len() }

    pub fn destroy(&mut self) {
        self.raw_file_data.clear();
        self.filenames.clear();
        self.header = ZosftHeader::default();
        self.footer = [0; ZOSFT_FOOTER_MAGICSIZE];
        for b in &mut self.blocks { b.destroy(); }
        self.file_index_map.clear();
        self.file_table.clear();
    }

    /// Log-only diagnostic: what format is each block section in?
    pub fn check_block_formats(&self) {
        for i in 0..ZOSFT_BLOCK_COUNT {
            for j in 0..3 {
                let Some(data) = self.blocks[i].get_data(j) else {
                    print_log!("{}-{}: No data in block!", i + 1, j + 1);
                    continue;
                };
                let Some(bytes) = data.bytes() else {
                    print_log!("{}-{}: No uncompressed data in block!", i + 1, j + 1);
                    continue;
                };
                if is_valid_snappy(bytes) {
                    print_log!("{}-{}: Snappy data!", i + 1, j + 1);
                } else if bytes.starts_with(b"\x78\x9C") {
                    print_log!("{}-{}: zLib data!", i + 1, j + 1);
                } else {
                    print_log!("{}-{}: Unknown data!", i + 1, j + 1);
                }
            }
        }
        self.test_count80(0, 0);
        self.test_count80(1, 0);
        self.test_count80(2, 0);
    }

    pub fn create_file_maps(&mut self) {
        self.file_index_map.clear();
        for (i, e) in self.file_table.iter().enumerate() {
            self.file_index_map.insert(e.file_index, i); // later duplicates win, like the C++ map
        }
    }

    pub fn create_file_table(&mut self) -> bool {
        self.file_table.clear();
        self.file_table.reserve(self.header.record_count as usize + 100);

        let get = |b: usize, j: usize| self.blocks[b].get_data(j).and_then(|d| d.bytes());
        let (Some(d11), Some(d13), Some(d21), Some(d23)) = (get(0, 0), get(0, 2), get(1, 0), get(1, 2)) else {
            print_error!("Error: Missing required data to generate ZOSFT table!");
            return false;
        };

        let (mut o11, mut o13, mut o21, mut o23) = (0usize, 0usize, 0usize, 0usize);
        // Declared outside the loop in the C++ (`Entry`), so the filename of the previous record
        // leaks into records that run out of block 2-3 data.  Kept.
        let mut filename = String::new();
        let mut table = Vec::with_capacity(self.header.record_count as usize);

        for i in 0..self.header.record_count {
            let mut e = ZosftFileEntry { index: i, ..Default::default() };

            if o11 < d11.len() {
                e.index11 = rd_u32(d11, o11);
                o11 = skip_to_next_marker(d11, o11);
            }
            if o13 < d13.len() {
                e.index13 = rd_u32(d13, o13);
                o13 += 4;
            }
            if o21 < d21.len() {
                e.index21 = rd_u32(d21, o21);
                o21 = skip_to_next_marker(d21, o21);
            }
            if o23 < d23.len() {
                e.file_index = rd_u32(d23, o23);
                o23 += 4;
                e.filename_offset = rd_u32(d23, o23);
                o23 += 4;
                e.file_id = rd_u64(d23, o23);
                o23 += 8;

                filename = if (e.filename_offset as usize) < self.raw_file_data.len() {
                    let rest = &self.raw_file_data[e.filename_offset as usize..];
                    let n = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
                    String::from_utf8_lossy(&rest[..n]).replace('/', "\\")
                } else {
                    String::new()
                };
            }
            e.filename = filename.clone();
            table.push(e);
        }

        if o11 != d11.len() { print_log!("Warning: Mismatch between parsed size ({}) and total size ({}) of block 1-1 data!", o11, d11.len()); }
        if o13 != d13.len() { print_log!("Warning: Mismatch between parsed size ({}) and total size ({}) of block 1-3 data!", o13, d13.len()); }
        if o21 != d21.len() { print_log!("Warning: Mismatch between parsed size ({}) and total size ({}) of block 2-1 data!", o21, d21.len()); }
        if o23 != d23.len() { print_log!("Warning: Mismatch between parsed size ({}) and total size ({}) of block 2-3 data!", o23, d23.len()); }

        self.file_table = table;
        self.create_file_maps();
        true
    }

    pub fn dump_file_table(&self, filename: &str) -> Result<()> {
        let out_path = extract_path(filename);
        if !out_path.is_empty() && !ensure_path_exists(&out_path) {
            return Err(crate::error::EsoError::new("could not create output path"));
        }
        let mut f = CFile::new();
        f.open(filename, "wb")?;

        cprintf!(f, "  Index,   Index11 ,   Index13 ,   Index21 , FileIndex,  FileOffset,      FileID       , UserData,                Filename\n");
        for e in &self.file_table {
            cprintf!(f, "{:7}, 0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:016X}, {:8}, '{}'\n",
                e.index as i32, e.index11, e.index13, e.index21, e.file_index, e.filename_offset, e.file_id, e.user_data as i32, e.filename);
        }
        Ok(())
    }

    pub fn load_file(&mut self, filename: &str) -> Result<()> {
        let mut f = CFile::new();
        f.open(filename, "rb")?;
        self.load(&mut f)
    }

    pub fn load(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        self.destroy();
        let start = get_timer_ms();

        self.read_header(file)?;
        self.read_block_data(file)?;
        self.read_file_data(file)?;
        self.read_footer(file)?;

        self.create_file_table(); // result ignored, like the C++

        print_log!("Successfully loaded ZOSFT in {} ms!", get_timer_ms() - start);
        Ok(())
    }

    /// Index into `file_table` of the entry with the given MNF file index.
    pub fn lookup_index(&self, file_index: u32) -> Option<usize> {
        self.file_index_map.get(&file_index).copied()
    }

    fn read_header(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        file.read_bytes(&mut self.header.file_id)?;
        if &self.header.file_id[..] != ZOSFT_MAGIC_ID {
            print_error!("Warning: Didn't find the expected '{}' at start of ZOSFT file!", String::from_utf8_lossy(ZOSFT_MAGIC_ID));
        }
        self.header.unknown1 = file.read_u16(true)?;
        self.header.unknown2 = file.read_u32(true)?;
        self.header.unknown3 = file.read_u32(true)?;
        self.header.record_count = file.read_u32(true)?;
        Ok(())
    }

    fn read_block_data(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        for b in &mut self.blocks {
            b.read(file, true)?;
        }
        Ok(())
    }

    fn read_file_data(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        let size = file.read_u32(true)? as usize;
        let mut buf = vec![0u8; size];
        file.read_bytes(&mut buf)?;
        self.raw_file_data = buf;
        self.split_raw_file_data();
        Ok(())
    }

    fn read_footer(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        file.read_bytes(&mut self.footer)?;
        if &self.footer[..] != ZOSFT_MAGIC_ID {
            print_error!("Warning: Didn't find the expected '{}' at end of ZOSFT file!", String::from_utf8_lossy(ZOSFT_MAGIC_ID));
        }
        let (size, pos) = (file.size(), file.tell());
        if size != pos {
            print_log!("Didn't reach exact end of ZOSFT file: {} bytes read, {} bytes total!", pos, size);
        } else {
            print_log!("Read all of ZOSFT file!");
        }
        Ok(())
    }

    fn split_raw_file_data(&mut self) {
        if self.raw_file_data.is_empty() { return; }
        let mut last = 0;
        for i in 0..self.raw_file_data.len() {
            if self.raw_file_data[i] == 0 {
                // includes the NUL, like `std::string(ptr, len + 1)`
                self.filenames.push(String::from_utf8_lossy(&self.raw_file_data[last..=i]).into_owned());
                last = i + 1;
            }
        }
        print_log!("Found {} filenames in raw ZOSFT file data!", self.filenames.len());
    }

    pub fn save_block(&self, block_index: usize, index: usize, filename: &str) -> Result<()> {
        if block_index >= ZOSFT_BLOCK_COUNT {
            return Err(crate::eso_error!("Error: Invalid ZOSFT block index {}!", block_index));
        }
        self.blocks[block_index].save_block(index, filename)
    }

    fn test_count80(&self, block_index: usize, index: usize) {
        if block_index >= ZOSFT_BLOCK_COUNT {
            print_log!("Error: Invalid block index {}!", block_index);
            return;
        }
        let Some(data) = self.blocks[block_index].get_data(index).and_then(|d| d.bytes()) else {
            print_log!("Error: Invalid block data index {}-{}!", block_index, index);
            return;
        };
        let count = data.chunks_exact(4).filter(|c| c[3] == 0x80).count();
        print_log!("{}-{}: Found {} 0x80XXXXXX records in block!", block_index + 1, index + 1, count);
    }

    /// For tests / future block readers: place already-decompressed block data.
    pub fn set_block(&mut self, i: usize, block: MnfBlock) { self.blocks[i] = block; }
    pub fn set_raw_file_data(&mut self, data: Vec<u8>) { self.raw_file_data = data; }
    pub fn set_record_count(&mut self, n: u32) { self.header.record_count = n; }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{MnfBlock, MnfBlockData, MnfBlockHeader};

    fn words(v: &[u32]) -> Vec<u8> { v.iter().flat_map(|w| w.to_le_bytes()).collect() }

    #[test]
    fn builds_file_table_from_blocks() {
        let mut z = ZosftFile::new(3);
        z.set_raw_file_data(b"\0/dir/a.lua\0/dir/b.lua\0".to_vec()); // offsets 1 and 12
        z.set_record_count(2);

        let mut b0 = MnfBlock::new(false, false);
        b0.set_contents(MnfBlockHeader::default(), vec![
            MnfBlockData::new(words(&[0x8000_0001, 0x8000_0002])), MnfBlockData::new(vec![]), MnfBlockData::new(words(&[7, 8])),
        ]);
        let mut b1 = MnfBlock::new(false, false);
        let mut d23 = words(&[100, 1, 0, 0]); // file_index, filename_offset, file_id(u64)
        d23.extend(words(&[200, 12, 0, 0]));
        b1.set_contents(MnfBlockHeader::default(), vec![
            MnfBlockData::new(words(&[0x8000_0003, 0x8000_0004])), MnfBlockData::new(vec![]), MnfBlockData::new(d23),
        ]);
        z.set_block(0, b0);
        z.set_block(1, b1);

        assert!(z.create_file_table());
        assert_eq!(z.file_table.len(), 2);
        assert_eq!(z.file_table[0].filename, "\\dir\\a.lua");
        assert_eq!(z.file_table[1].file_index, 200);
        assert_eq!(z.file_table[1].index13, 8);
        assert_eq!(z.lookup_index(200), Some(1));
    }
}
