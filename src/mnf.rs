//! Port of EsoMnfFile.cpp (`CMnfFile`): loads an ESO `.mnf` index, links it to the ZOSFT filename table
//! and exports the sub-files stored in the numbered `.dat` archives.
//!
//! The block reader ([`crate::block`]) and the sub-file data/index parsers ([`crate::subfile`]) are
//! ported.  Still missing because the original sources were not supplied: the ww2ogg RIFF->OGG
//! conversion.  Assumed header constants are marked `TODO verify`.
//!
//! Differences from the C++:
//! * The file table is never copied; sorting works on index vectors and `MnfFileEntry` is `Copy`.
//! * ZOSFT/GR2 names that would escape the output directory (`..`, absolute) are skipped.
//! * Windows SEH (`__try/__except`) around the Granny call cannot be reproduced; a crash inside the
//!   Granny library will take the process down.

use crate::block::MnfBlock;
use crate::dat::{read_sub_file_data, DatSubFileInfo};
use crate::error::{EsoError, Result};
use crate::file::{BaseFile, CFile, MemoryFile};
use crate::granny;
use crate::lang::EsoLangFile;
use crate::oodle::oodle_decompress;
use crate::subfile::{EsoSubFileDataFile, EsoSubFileIndexFile};
use crate::util::*;
use crate::zosft::ZosftFile;
use crate::{eso_debug_error, eso_error, print_debug, print_error, print_log};
use std::collections::HashMap;
use std::path::MAIN_SEPARATOR as SEP;

pub const MNF_MAGIC_ID: &[u8; 4] = b"MES2"; // TODO verify
pub const MNF_HEADER_MAGICSIZE: usize = 4; // TODO verify
pub const MNF_BLOCK1_RECORDSIZE: usize = 4; // inferred from the parsing code
pub const MNF_BLOCK2_RECORDSIZE: usize = 8; // inferred
pub const MNF_BLOCK3_RECORDSIZE: usize = 20; // inferred
/// `MNF_ESOZOSFT_HASH` / `MNF_GAMEZOSFT_HASH` live in the unsupplied header; the explicit hashes below still work.
pub const MNF_ESOZOSFT_HASH: Option<u32> = None; // TODO fill in from EsoMnfFile.h
pub const MNF_GAMEZOSFT_HASH: Option<u32> = None; // TODO fill in from EsoMnfFile.h

#[derive(Debug, Clone)]
pub struct MnfHeader {
    pub file_id: [u8; MNF_HEADER_MAGICSIZE],
    pub data_size: u32,
    pub file_count: u32,
    pub version: u16,
    /// MNF file type (6 is the type with the data1/data2 sub-headers).
    pub type_: u32,
    pub file_types: Vec<u16>,
    pub unknown1: u16,
}

impl Default for MnfHeader {
    fn default() -> Self {
        Self { file_id: *MNF_MAGIC_ID, data_size: 0, file_count: 0, version: 2, type_: 1, file_types: Vec::new(), unknown1: 0 }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MnfFileEntry {
    pub index: u32,
    pub id1: u32,
    pub file_index: u32,
    pub unknown1: u32,
    pub size: u32,
    pub compressed_size: u32,
    pub hash: u32,
    pub offset: u32,
    pub compress_type: u8,
    pub archive_index: u8,
    pub unknown2: u16,
    /// Index into the loaded ZOSFT file table (`pZosftEntry`).
    pub zosft_index: Option<usize>,
}

impl MnfFileEntry {
    /// The real file index: `unknown1` is an offset that is added to `file_index`.
    pub fn true_index(&self) -> u32 {
        self.file_index.wrapping_add(self.unknown1)
    }
}

#[derive(Debug, Clone, Default)]
pub struct MnfExportOptions {
    pub mnf_filename: String,
    pub output_path: String,
    pub mnf_output_file_table: String,
    pub zos_output_file_table: String,

    pub mnf_start_index: i32,
    pub mnf_end_index: i32,
    pub archive_index: i32,
    pub begin_archive_index: i32,
    pub mnf_file_indices: Vec<u32>,

    pub convert_dds: bool,
    pub skip_sub_files: bool,
    pub no_parse_gr2: bool,
    pub oodle_raw_output: bool,
    pub no_riff_convert: bool,
    pub debug_output: bool,

    pub use_lang_text: bool,
    pub use_po_csv_format: bool,
    pub use_po_source_text: bool,

    pub lang_filename: String,
    pub create_lang_filename: String,
    pub base_lang_filename: String,
    pub output_filename: String,
    pub import_id_filename: String,
    pub diff_lang_filename1: String,
    pub diff_lang_filename2: String,
    pub id_filename1: String,
    pub id_filename2: String,
    pub orig_lang_filename: String,

    pub extract_filename: String,
    pub extract_file_extension: String,
    pub match_filename: String,
    pub extract_sub_file_data_type: String,

    pub lua_file_list: String,
    pub lua_start_index: i32,
}

/// The trailing parameters of `CMnfFile::SaveSubFile` (all defaulted in the C++).
#[derive(Debug, Clone, Default)]
pub struct SaveSubFileOptions {
    pub extract_sub_file_data_type: String,
    pub no_extract_gr2: bool,
    pub extract_file_extension: String,
    pub no_convert_riff: bool,
    pub match_filename: String,
}

impl SaveSubFileOptions {
    pub fn from_export(o: &MnfExportOptions) -> Self {
        Self {
            extract_sub_file_data_type: o.extract_sub_file_data_type.clone(),
            no_extract_gr2: o.no_parse_gr2,
            extract_file_extension: o.extract_file_extension.clone(),
            no_convert_riff: o.no_riff_convert,
            match_filename: o.match_filename.clone(),
        }
    }
}

/// Name of the first item of a Granny array (null-safe).
///
/// # Safety
/// `items` must be null or point to `count` valid item pointers.
unsafe fn first_name<T>(count: i32, items: *mut *mut T, name: impl Fn(&T) -> *mut std::ffi::c_char) -> String {
    if count > 0 && !items.is_null() {
        // Granny packs its structures on 4-byte boundaries, so the pointer array may not be 8-aligned.
        let first = std::ptr::read_unaligned(items);
        if !first.is_null() {
            return granny::cstr_lossy(name(&*first));
        }
    }
    String::new()
}

fn failed(msg: &str) -> EsoError {
    EsoError::new(msg)
}

/// The header test `parse_data_file_ver3` uses before attempting Oodle decompression.
fn looks_oodle_compressed(data: &[u8]) -> bool {
    data.len() >= 2 && (data[0] == 0x8C || data[0] == 0xCC) && (data[1] == 0x06 || data[1] == 0x0A)
}

/// `CreateFilename(base, "%03u\\%06u<tail>", archive, index)`
fn numeric_name(base: &str, archive: u8, index: u32, tail: &str) -> String {
    create_filename(base, &format!("{:03}{}{:06}{}", archive, SEP, index, tail))
}

#[derive(Debug)]
pub struct MnfFile {
    filename: String,
    header: MnfHeader,
    has_block0: bool,
    block0: MnfBlock,
    block3: MnfBlock,
    file_table: Vec<MnfFileEntry>,
    hash_map: HashMap<u32, usize>,
    index_map: HashMap<u32, usize>,
    internal_index_map: HashMap<u32, usize>,
    duplicate_name_map: HashMap<String, u32>,
    decompress_oodle: bool,
    zosft_file: ZosftFile,
    zosft_hash: u32,
    ww2ogg_packed_bin_filename: String,
}

impl Default for MnfFile {
    fn default() -> Self {
        Self::new()
    }
}

impl MnfFile {
    pub fn new() -> Self {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
        Self {
            filename: String::new(),
            header: MnfHeader::default(),
            has_block0: false,
            block0: MnfBlock::new_block0(false, false),
            block3: MnfBlock::new(false, false),
            file_table: Vec::new(),
            hash_map: HashMap::new(),
            index_map: HashMap::new(),
            internal_index_map: HashMap::new(),
            duplicate_name_map: HashMap::new(),
            decompress_oodle: true,
            zosft_file: ZosftFile::new(0),
            zosft_hash: 0,
            ww2ogg_packed_bin_filename: exe_dir.join("packed_codebooks_aoTuV_603.bin").to_string_lossy().into_owned(),
        }
    }

    pub fn file_table(&self) -> &[MnfFileEntry] { &self.file_table }
    pub fn header(&self) -> &MnfHeader { &self.header }
    pub fn zosft(&self) -> &ZosftFile { &self.zosft_file }

    pub fn destroy(&mut self) {
        self.filename.clear();
        self.file_table.clear();
        self.hash_map.clear();
        self.index_map.clear();
        self.internal_index_map.clear();
        self.block0.destroy();
        self.block3.destroy();
        self.has_block0 = false;
        self.header.data_size = 0;
        self.header.file_count = 0;
        self.zosft_hash = 0;
        self.zosft_file.destroy();
    }

    /// Filename (from the ZOSFT) of an entry, if linked and non-empty.
    pub fn zosft_name(&self, e: &MnfFileEntry) -> Option<&str> {
        e.zosft_index.and_then(|i| self.zosft_file.file_table.get(i)).map(|z| z.filename.as_str())
    }

    // -----------------------------------------------------------------------------------------
    // File table
    // -----------------------------------------------------------------------------------------

    pub fn create_file_table(&mut self) -> Result<()> {
        let (Some(d1), Some(d2), Some(d3)) = (
            self.block3.get_data(0).and_then(|d| d.bytes()),
            self.block3.get_data(1).and_then(|d| d.bytes()),
            self.block3.get_data(2).and_then(|d| d.bytes()),
        ) else {
            return Err(eso_error!("Error: Missing one or more block data(s) required to create MNF file table!"));
        };

        let start = get_timer_ms();
        print_log!("Creating MNF file table....");

        if d1.len() % MNF_BLOCK1_RECORDSIZE != 0 { print_log!("Warning: MNF Block1 ({} bytes) is not an even multiple of the record size {}!", d1.len(), MNF_BLOCK1_RECORDSIZE); }
        if d2.len() % MNF_BLOCK2_RECORDSIZE != 0 { print_log!("Warning: MNF Block2 ({} bytes) is not an even multiple of the record size {}!", d2.len(), MNF_BLOCK2_RECORDSIZE); }
        if d3.len() % MNF_BLOCK3_RECORDSIZE != 0 { print_log!("Warning: MNF Block3 ({} bytes) is not an even multiple of the record size {}!", d3.len(), MNF_BLOCK3_RECORDSIZE); }

        let rd32 = |d: &[u8], o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        let (mut o1, mut o2, mut o3) = (0usize, 0usize, 0usize);
        let mut table = Vec::new();

        for i in 0..self.block3.header().record_count23 {
            let mut e = MnfFileEntry { index: i, ..Default::default() };

            if o1 + MNF_BLOCK1_RECORDSIZE <= d1.len() {
                e.id1 = rd32(d1, o1);
                loop {
                    o1 += MNF_BLOCK1_RECORDSIZE;
                    if !(o1 + MNF_BLOCK1_RECORDSIZE <= d1.len() && d1[o1 + 3] != 0x80) { break; }
                }
            }

            if o2 + MNF_BLOCK2_RECORDSIZE <= d2.len() {
                e.file_index = rd32(d2, o2);
                e.unknown1 = rd32(d2, o2 + 4);
                o2 += MNF_BLOCK2_RECORDSIZE;
            }

            if o3 + MNF_BLOCK3_RECORDSIZE <= d3.len() {
                e.size = rd32(d3, o3);
                e.compressed_size = rd32(d3, o3 + 4);
                e.hash = rd32(d3, o3 + 8);
                e.offset = rd32(d3, o3 + 12);
                e.compress_type = d3[o3 + 16];
                e.archive_index = d3[o3 + 17];
                e.unknown2 = u16::from_le_bytes([d3[o3 + 18], d3[o3 + 19]]);
                o3 += MNF_BLOCK3_RECORDSIZE;

                if self.header.version >= 3 {
                    std::mem::swap(&mut e.compress_type, &mut e.archive_index);
                }
            }
            table.push(e);
        }

        if d1.len() != o1 { print_log!("Warning: Did not fully parse MNF Block1: only {} of {} bytes read!", o1, d1.len()); }
        if d2.len() != o2 { print_log!("Warning: Did not fully parse MNF Block2: only {} of {} bytes read!", o2, d2.len()); }
        if d3.len() != o3 { print_log!("Warning: Did not fully parse MNF Block3: only {} of {} bytes read!", o3, d3.len()); }

        self.file_table.extend(table);
        self.create_file_maps();

        print_log!("MNF filetable filled with {} entries in {} ms!", self.file_table.len(), get_timer_ms() - start);
        Ok(())
    }

    pub fn create_file_maps(&mut self) {
        self.hash_map.clear();
        self.index_map.clear();
        self.internal_index_map.clear();
        print_log!("File Index Count = {}", self.file_table.len());
        for (i, e) in self.file_table.iter().enumerate() {
            self.hash_map.insert(e.hash, i);
            self.index_map.insert(e.true_index(), i);
            self.internal_index_map.insert(e.index, i);
        }
    }

    pub fn create_duplicate_map(&mut self, zosft: &ZosftFile) {
        for e in &self.file_table {
            let Some(z) = e.zosft_index.and_then(|i| zosft.file_table.get(i)) else { continue };
            if z.user_data <= 1 { continue; }
            let cur = self.duplicate_name_map.entry(z.filename.clone()).or_insert(0);
            if *cur < e.index { *cur = e.index; }
        }
    }

    pub fn dump_file_table(&self, filename: &str) -> Result<()> {
        let out_path = extract_path(filename);
        if !out_path.is_empty() && !ensure_path_exists(&out_path) {
            return Err(failed("could not create output path"));
        }
        let mut f = CFile::new();
        f.open(filename, "wb")?;

        crate::cprintf!(f, " Index ,    ID1    , FileIndex ,   Unk1    ,    Size   ,   ZSize   ,    Hash   ,   Offset  , ZTyp, Arch,  Unk2 , Filename\n");
        for e in &self.file_table {
            crate::cprintf!(f, "{:7}, 0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:08X}, 0x{:02X}, 0x{:02X}, 0x{:04X}, {}\n",
                e.index as i32, e.id1, e.file_index, e.unknown1, e.size, e.compressed_size, e.hash, e.offset,
                e.compress_type, e.archive_index, e.unknown2, self.zosft_name(e).unwrap_or(""));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // ZOSFT lookup / loading
    // -----------------------------------------------------------------------------------------

    fn candidate_hashes(&self) -> (bool, bool) {
        if string_ends_with(&self.filename, "eso.mnf") {
            (true, false)
        } else if string_ends_with(&self.filename, "game.mnf") {
            (false, true)
        } else {
            (true, true)
        }
    }

    fn get_zosft_file_entry(&self) -> Option<usize> {
        let (check_eso, check_game) = self.candidate_hashes();

        if check_eso {
            for key in [0x00FF_FFFFu32, 0xFFFFF] {
                if let Some(&i) = self.index_map.get(&key) { return Some(i); }
            }
            for h in MNF_ESOZOSFT_HASH.into_iter().chain([0xF4FD_2ECD, 0x4C99_266E, 0x8F20_34EC]) {
                if let Some(&i) = self.hash_map.get(&h) { return Some(i); }
            }
        }
        if check_game {
            if let Some(&i) = self.index_map.get(&0) { return Some(i); }
            for h in MNF_GAMEZOSFT_HASH.into_iter().chain([0xF969_BEE8, 0xA16B_BD50, 0x3001_A8A2]) {
                if let Some(&i) = self.hash_map.get(&h) { return Some(i); }
            }
        }
        None
    }

    /// Unused in the C++ (its call site is commented out); kept for completeness.
    pub fn find_zosft_hash(&self) -> Option<u32> {
        let (check_eso, check_game) = self.candidate_hashes();

        if check_eso {
            for key in [0x00FF_FFFFu32, 0xFFFFF] {
                if let Some(&i) = self.index_map.get(&key) { return Some(self.file_table[i].hash); }
            }
            for h in MNF_ESOZOSFT_HASH.into_iter().chain([0xF4FD_2ECD, 0x4C99_266E, 0x8F20_34EC]) {
                if self.hash_map.contains_key(&h) { return Some(h); }
            }
        }
        if check_game {
            if let Some(&i) = self.index_map.get(&0) { return Some(self.file_table[i].hash); }
            for h in MNF_GAMEZOSFT_HASH.into_iter().chain([0xF969_BEE8, 0xA16B_BD50, 0x3001_A8A2]) {
                if self.hash_map.contains_key(&h) { return Some(h); }
            }
        }
        None
    }

    pub fn load_zosft(&mut self) -> Result<()> {
        self.zosft_file.destroy();
        self.zosft_file.set_version(self.header.version as u32);
        self.zosft_hash = 0;

        let Some(entry) = self.get_zosft_file_entry() else {
            return Err(eso_error!("ERROR: Failed to find the ZOSFT entry in the MNF file!"));
        };

        // `link_to_zosft` needs `&mut self` and the ZOSFT at the same time.
        let mut zosft = std::mem::take(&mut self.zosft_file);
        let result = self.load_zosft_entry(entry, &mut zosft);
        self.zosft_file = zosft;

        result.map_err(|_| eso_error!("Failed to load the ZOSFT data from MNF file data!\nFiles will be exported but without any filenames set."))
    }

    fn load_zosft_entry(&mut self, entry_index: usize, zosft: &mut ZosftFile) -> Result<()> {
        let entry = self.file_table[entry_index];
        let mut info = DatSubFileInfo::default();

        print_log!("Trying to load ZOSFT sub-file identified by hash 0x{:08X}...", entry.hash);
        self.read_data_file(&entry, &mut info, None)?;

        let mut file = MemoryFile::from_vec(info.file_bytes().to_vec());
        if let Err(e) = zosft.load(&mut file) {
            print_log!("Failed to load ZOSFT sub-file identified by hash 0x{:08X}!", entry.hash);
            return Err(e);
        }

        self.link_to_zosft(zosft);
        self.create_duplicate_map(zosft);
        Ok(())
    }

    /// Variant that looks the ZOSFT up by hash (unused by the exporter).
    pub fn load_zosft_by_hash(&mut self, hash: u32, zosft: &mut ZosftFile) -> Result<()> {
        print_log!("Trying to load ZOSFT sub-file identified by hash 0x{:08X}...", hash);
        let Some(&idx) = self.hash_map.get(&hash) else {
            return Err(eso_error!("Error: The file hash 0x{:08X} was not found in MNF file table!", hash));
        };
        self.load_zosft_entry(idx, zosft)
    }

    pub fn link_to_zosft(&mut self, zosft: &mut ZosftFile) {
        let mut found = 0usize;
        print_log!("Looking up all MNF entries in given ZOSFT file...");

        for e in &mut self.file_table {
            if let Some(zi) = zosft.lookup_index(e.true_index()) {
                zosft.file_table[zi].user_data += 1;
                e.zosft_index = Some(zi);
                found += 1;
            }
        }
        print_log!("Found {} of {} MNF entries in ZSOFT file!", found, self.file_table.len());
    }

    // -----------------------------------------------------------------------------------------
    // Loading the MNF
    // -----------------------------------------------------------------------------------------

    pub fn load(&mut self, filename: &str) -> Result<()> {
        let mut file = CFile::new();
        file.open(filename, "rb")?;
        let result = self.load_from(&mut file);
        self.filename = filename.to_string(); // set after loading, as in the C++
        result
    }

    pub fn load_from(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        self.destroy();
        let start = get_timer_ms();
        self.read_header(file)?;
        self.read_blocks(file)?;
        self.create_file_table()?;
        print_log!("Loaded MNF file in {} ms!", get_timer_ms() - start);
        Ok(())
    }

    fn read_header(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        file.read_bytes(&mut self.header.file_id)?;
        if &self.header.file_id != MNF_MAGIC_ID {
            print_log!("Warning: Did not find the magic bytes of '{}' at start of MNF file!", String::from_utf8_lossy(MNF_MAGIC_ID));
        }
        self.header.version = file.read_u16(true)?;
        if self.header.version >= 3 {
            return self.read_header_version3(file);
        }
        self.header.file_count = file.read_u8()? as u32;
        self.header.type_ = file.read_u32(true)?;
        self.header.data_size = file.read_u32(true)?;
        Ok(())
    }

    fn read_header_version3(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        self.header.file_count = file.read_u32(true)?;
        self.header.file_types.clear();
        for _ in 0..self.header.file_count {
            let t = file.read_u16(true)?;
            self.header.file_types.push(t);
        }
        self.header.unknown1 = file.read_u16(true)?;
        self.header.data_size = file.read_u32(true)?;
        Ok(())
    }

    /// For now try to read an optional block 0 followed by a block 3.
    fn read_blocks(&mut self, file: &mut dyn BaseFile) -> Result<()> {
        let mut block_id = file.read_u16(false)?;

        if block_id == 0 {
            self.has_block0 = true;
            self.block0.read(file, false)?;
            block_id = file.read_u16(false)?;
        }

        if block_id == 3 {
            self.has_block0 = false;
            self.block3.read(file, false)?;
        } else {
            let pos = file.tell();
            return Err(eso_error!("Error: Found unknown block type {} in MNF file at 0x{:x}!", block_id, pos));
        }

        let (cur, size) = (file.tell(), file.size());
        if cur < size {
            print_log!("Warning: Extra {} bytes found at end of MNF file (read {} of {} bytes)!", size - cur, cur, size);
        } else {
            print_log!("No bytes left over in MNF file!");
        }
        Ok(())
    }

    pub fn save_block(&self, block_type: usize, index: usize, filename: &str) -> Result<()> {
        match block_type {
            0 => self.block0.save_block(index, filename),
            3 => self.block3.save_block(index, filename),
            _ => Err(eso_error!("Error: Unknown block type {}!", block_type)),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Reading sub-file data
    // -----------------------------------------------------------------------------------------

    pub fn create_data_filename(&self, archive_index: u8) -> String {
        create_filename(&remove_file_extension(&self.filename), &format!("{:04}.dat", archive_index))
    }

    pub fn read_data_file(&self, entry: &MnfFileEntry, info: &mut DatSubFileInfo, file: Option<&mut CFile>) -> Result<()> {
        info.filename = self.create_data_filename(entry.archive_index);
        info.compress_type = entry.compress_type as u32;
        info.archive_index = entry.archive_index;
        info.raw_size = entry.compressed_size;
        info.uncompressed_size = entry.size;
        info.offset = entry.offset as u64;
        info.uncompressed_data = None;
        info.raw_data = None;

        read_sub_file_data(info, &self.header, file)?;

        if self.header.version >= 3 {
            self.parse_data_file_ver3(entry, info); // result intentionally ignored (as in the C++)
        }
        Ok(())
    }

    fn parse_data_file_ver3(&self, entry: &MnfFileEntry, info: &mut DatSubFileInfo) -> bool {
        if !info.has_file_data() { return true; }

        {
            if !looks_oodle_compressed(info.file_bytes()) { return true; }
        }
        if !self.decompress_oodle { return true; }

        let mut out = vec![0u8; entry.size as usize + 10000];
        let n = match oodle_decompress(info.file_bytes(), &mut out, entry.size as usize) {
            Ok(n) => n,
            Err(_) => {
                print_debug!("\t{}: Warning: Failed to decompress V3 data file!", entry.true_index());
                return false;
            }
        };

        info.oodle_compressed = info.file_data.clone();
        out.truncate(n);
        info.oodle_decompressed = Some(out);
        info.file_data = 0..n;

        let o = info.oodle_decompressed.as_deref().unwrap();
        if n < 4 || o[..4] != [0, 0, 0, 0] { return true; }

        // `word HeaderOffset1 = ParseBufferWord(...) + 8;` truncates to 16 bits.
        let header_offset1 = parse_buffer_word(&o[6.min(n)..], true).wrapping_add(8) as usize;
        if header_offset1 >= n {
            print_log!("\t\tHeader Offset #1 in V3 DAT file exceeds file size (0x{:04X} > 0x{:08X})!", header_offset1, n);
            return false;
        }

        let header_offset2 = parse_buffer_dword(&o[header_offset1..], true).wrapping_add(4).wrapping_add(header_offset1 as u32) as usize;
        if header_offset2 >= n {
            print_log!("\t\tHeader Offset #2 in V3 DAT file exceeds file size (0x{:04X} > 0x{:08X})!", header_offset2, n);
            return false;
        }

        info.file_data = header_offset2..n;
        true
    }

    // -----------------------------------------------------------------------------------------
    // In-memory reads
    // -----------------------------------------------------------------------------------------

    /// Reads and decompresses one entry into memory (zlib, snappy or none; Oodle for v3 archives).
    ///
    /// `file` is an open DAT for the entry's archive (see [`create_data_filename`](Self::create_data_filename));
    /// pass `None` to open it for this call. Fails rather than returning the still-compressed bytes
    /// when an Oodle-compressed entry could not be decompressed (typically because the Oodle
    /// library is not loaded).
    pub fn read_entry(&self, e: &MnfFileEntry, file: Option<&mut CFile>) -> Result<Vec<u8>> {
        let mut info = DatSubFileInfo::default();
        self.read_data_file(e, &mut info, file)?;
        if !info.has_file_data() {
            return Err(failed("entry has no data"));
        }
        if self.header.version >= 3 && info.oodle_decompressed.is_none() && looks_oodle_compressed(info.file_bytes()) {
            return Err(failed("entry is Oodle-compressed but could not be decompressed (is the Oodle library loaded?)"));
        }
        Ok(info.file_bytes().to_vec())
    }

    /// Reads an EsoFileData table entry (the source of the `*_Uncompressed.EsoFileData` exports),
    /// with its index records attached exactly as the file exports do.
    pub fn read_eso_file_data(&self, e: &MnfFileEntry) -> Result<EsoSubFileDataFile> {
        let mut info = DatSubFileInfo::default();
        self.read_data_file(e, &mut info, None)?;
        self.extract_sub_file_data(e, "", &info)
    }

    /// Reads several entries without holding them all in memory: `f` is called once per distinct
    /// requested index, with its bytes or the reason it could not be read.
    ///
    /// `indices` are absolute entry indices (`MnfFileEntry::index`, the position in
    /// [`file_table`](Self::file_table)). Entries are read in archive/offset order and each DAT is
    /// opened once, so the cost is one pass over each archive. A failing entry is reported to `f`
    /// and never stops the batch.
    pub fn read_entries_each(&self, indices: &[u32], mut f: impl FnMut(u32, Result<Vec<u8>>)) {
        let mut seen = std::collections::HashSet::new();
        let mut found: Vec<(u32, usize)> = Vec::with_capacity(indices.len());

        for &idx in indices {
            if !seen.insert(idx) { continue; }
            match self.file_table.get(idx as usize) {
                Some(e) if e.index == idx => found.push((idx, idx as usize)),
                _ => f(idx, Err(failed(&format!("entry {} is not in the MNF file table", idx)))),
            }
        }

        found.sort_by_key(|&(_, i)| (self.file_table[i].archive_index, self.file_table[i].offset));

        let mut input = CFile::new();
        let mut open_archive: Option<u8> = None;
        let mut dat_ok = false;

        for (idx, i) in found {
            let entry = self.file_table[i];
            if open_archive != Some(entry.archive_index) {
                open_archive = Some(entry.archive_index);
                input.close();
                let name = self.create_data_filename(entry.archive_index);
                dat_ok = input.open(&name, "rb").is_ok();
            }
            if !dat_ok {
                f(idx, Err(failed("could not open the DAT archive for this entry")));
                continue;
            }
            f(idx, self.read_entry(&entry, Some(&mut input)));
        }
    }

    // -----------------------------------------------------------------------------------------
    // Export entry point
    // -----------------------------------------------------------------------------------------

    pub fn export(&mut self, opts: &MnfExportOptions) -> Result<()> {
        print_error!("Loading MNF file '{}'...", opts.mnf_filename);

        self.decompress_oodle = !opts.oodle_raw_output;

        if self.load(&opts.mnf_filename).is_err() {
            print_error!("Failed to load MNF file '{}'...aborting!", opts.mnf_filename);
            return Err(failed("failed to load MNF file"));
        }

        print_error!("Trying to find and load ZOSFT entry from MNF file!");
        if self.load_zosft().is_ok() {
            print_error!("Successfully loaded the ZOSFT entry for MNF file!");
        }

        if !opts.mnf_output_file_table.is_empty() {
            if self.dump_file_table(&opts.mnf_output_file_table).is_err() {
                print_error!("Error: Failed to dump the MNF filetable to a text file '{}'!", opts.mnf_output_file_table);
            } else {
                print_error!("Saved the MNF filetable to {}!", opts.mnf_output_file_table);
            }
        }

        if !opts.zos_output_file_table.is_empty() {
            if self.zosft_file.dump_file_table(&opts.zos_output_file_table).is_err() {
                print_error!("Error: Failed to dump the ZOS filetable to a text file '{}'!", opts.zos_output_file_table);
            } else {
                print_error!("Saved the ZOSFT to {}!", opts.zos_output_file_table);
            }
        }

        if !opts.lua_file_list.is_empty() {
            return self.export_lua_file_list(opts);
        }

        if opts.skip_sub_files {
            print_error!("Skipping extraction of subfile data....\n");
            return Ok(());
        }

        if !opts.extract_filename.is_empty() {
            return self.extract_specific_file(&opts.extract_filename, opts);
        }

        if !opts.mnf_file_indices.is_empty() {
            return self.save_sub_files_by_index(&opts.mnf_file_indices, opts);
        }
        self.save_sub_files(opts)
    }

    pub fn export_lua_file_list(&self, opts: &MnfExportOptions) -> Result<()> {
        let mut index = opts.lua_start_index;
        print_error!("Saving list of files to LUA file '{}'...\n", opts.lua_file_list);

        if !opts.extract_file_extension.is_empty() {
            print_error!("Only outputting files with extension of '{}'...\n", opts.extract_file_extension);
        }
        if !opts.match_filename.is_empty() {
            print_error!("Only outputting files containing the substring of '{}'...\n", opts.match_filename);
        }

        let mut f = CFile::new();
        f.open(&opts.lua_file_list, "wb")?;

        for e in &self.file_table {
            let Some(name) = self.zosft_name(e) else { continue };
            if !opts.extract_file_extension.is_empty() && !string_ends_with(name, &opts.extract_file_extension) { continue; }
            if !opts.match_filename.is_empty() && !name.contains(&opts.match_filename) { continue; }

            crate::cprintf!(f, "\t[{}] = \"{}\",\n", index, name.replace('\\', "/"));
            index += 1;
        }
        Ok(())
    }

    pub fn extract_specific_file(&self, filename: &str, opts: &MnfExportOptions) -> Result<()> {
        let target = filename.to_lowercase();
        let mut ok = true;
        print_error!("Extracting any filename matching '{}'...", target);

        for e in &self.file_table {
            let (mut full, mut name, mut no_ext) = (String::new(), String::new(), String::new());

            if let Some(n) = self.zosft_name(e).filter(|n| !n.is_empty()) {
                full = n.to_lowercase();
                name = full.rsplit(['\\', '/']).next().unwrap_or("").to_string();
                no_ext = name.rfind('.').map_or(name.clone(), |p| name[..p].to_string());
            }

            if name == target || full == target || no_ext == target || target == e.index.to_string() {
                print_error!("Extracting matching subfile #{}!", e.index);
                ok &= self.save_sub_file(e, &opts.output_path, opts.convert_dds, None, &SaveSubFileOptions::default()).is_ok();
            }
        }
        if ok { Ok(()) } else { Err(failed("some matching sub-files could not be exported")) }
    }

    // -----------------------------------------------------------------------------------------
    // Saving sub-files
    // -----------------------------------------------------------------------------------------

    fn write_file(path: &str, data: &[u8]) -> Result<()> {
        let mut f = CFile::new();
        f.open(path, "wb")?;
        f.write_bytes(data)
    }

    fn save_sub_file_zosft(&self, e: &MnfFileEntry, base_path: &str, convert_dds: bool, info: &DatSubFileInfo) -> Result<()> {
        let name = self.zosft_name(e).unwrap_or("");
        if has_unsafe_component(name) {
            return Err(eso_error!("\tWarning: Skipping ZOSFT filename '{}' that is outside the output path!", name));
        }
        let output_filename = append_filename_to_path(base_path, name);
        let output_path = remove_filename(&output_filename);

        if !ensure_path_exists(&output_path) { return Err(failed("could not create output path")); }

        let valid_index = self.duplicate_name_map.get(name).copied().unwrap_or(0);
        if valid_index > 0 && e.index != valid_index {
            return Err(eso_debug_error!("\tWarning: Skipping duplicate file '{}' with index {}!", output_filename, e.index));
        }

        Self::write_file(&output_filename, info.file_bytes())?;

        if convert_dds && string_ends_with(&output_filename, ".dds") {
            let _ = convert_dds_to_png(info.file_bytes(), &output_filename);
        } else if string_ends_with(&output_filename, ".lang") {
            let mut lang = EsoLangFile::new();
            if lang.load(&output_filename).is_ok() {
                let _ = lang.dump_csv(&format!("{}.csv", output_filename), false);
            }
        }
        Ok(())
    }

    fn extract_sub_file_data(&self, e: &MnfFileEntry, base_path: &str, info: &DatSubFileInfo) -> Result<EsoSubFileDataFile> {
        let mut data = EsoSubFileDataFile::default();
        data.parse(info.file_bytes())?;

        if let Some(&idx) = self.internal_index_map.get(&(e.index + 1)) {
            let index_entry = self.file_table[idx];
            let mut index_info = DatSubFileInfo::default();
            if self.read_data_file(&index_entry, &mut index_info, None).is_ok() {
                let mut index = EsoSubFileIndexFile::default();
                if index.parse(index_info.file_bytes()).is_ok() {
                    data.update_index(index.records());
                }
            }
        }
        let _ = base_path;
        Ok(data)
    }

    fn extract_sub_file_data_combined(&self, e: &MnfFileEntry, base_path: &str, info: &DatSubFileInfo) -> Result<()> {
        let data = self.extract_sub_file_data(e, base_path, info)?;
        data.save_combined_file(&numeric_name(base_path, e.archive_index, e.index, "_Uncompressed.EsoFileData"))
    }

    fn extract_sub_file_data_seperate(&self, e: &MnfFileEntry, base_path: &str, info: &DatSubFileInfo) -> Result<()> {
        let data = self.extract_sub_file_data(e, base_path, info)?;
        let out_path = numeric_name(base_path, e.archive_index, e.index, &SEP.to_string());
        if !ensure_path_exists(&out_path) { return Err(failed("could not create output path")); }
        data.save_files(&out_path)
    }

    fn save_sub_file_gr2(&self, e: &MnfFileEntry, base_path: &str, info: &DatSubFileInfo) -> Result<()> {
        let id = format!("{:03}{}{:06}.gr2", e.archive_index, SEP, e.index);

        let Some(api) = granny::global_api() else {
            return Err(eso_error!("\tError: The Granny library is not available (file {})!", id));
        };
        let file = api.read_from_memory(info.file_bytes())
            .map_err(|_| eso_error!("\tError: Failed to parse Granny file data (file {})!", id))?;

        let fi = file.file_info();
        if fi.is_null() {
            return Err(eso_error!("\tError: Failed to get Granny file info (file {})!", id));
        }

        // SAFETY: `fi` is non-null and stays valid while `file` lives; every string is copied before `file` is dropped.
        let (orig_file, model_name, mesh_name, mut anim_name) = unsafe {
            let fi = &*fi;
            (
                granny::cstr_lossy(fi.FromFileName),
                first_name(fi.nModels, fi.Models, |m: &granny::Model_t| m.Name),
                first_name(fi.nMeshes, fi.Meshes, |m: &granny::Mesh_t| m.Name),
                first_name(fi.nAnimations, fi.Animations, |a: &granny::Animation_t| a.Name),
            )
        };
        drop(file);

        let reconstructed = format!("{}Granny{}Reconstructed{}", base_path, SEP, SEP);

        // Use the alternative reconstruction method if no filename is available or it is only digits
        if orig_file.is_empty() || orig_file.chars().all(|c| c.is_ascii_digit()) {
            let mut out = reconstructed.clone();
            if !model_name.is_empty() { out.push_str(&format!("Mdl_{}-", model_name)); }
            if !mesh_name.is_empty() { out.push_str(&format!("Msh_{}-", mesh_name)); }
            if !anim_name.is_empty() {
                // Some animation names contain the entire path; keep only the filename.
                if let Some(p) = anim_name.rfind('\\') { anim_name.drain(..=p); }
                out.push_str(&format!("Anm_{}-", anim_name));
            }

            if out == reconstructed {
                return Err(eso_error!("\tWarning: No original file, model, mesh or animation name found in Granny file data (file {})!", id));
            } else if !orig_file.is_empty() {
                out.pop(); // remove last '-'
                out.push_str(&format!("[{}]", orig_file));
            }

            out = out.replace('|', "_"); // invalid in filenames
            out.push_str(".gr2");

            let out_path = remove_filename(&out);
            if !ensure_path_exists(&out_path) { return Err(failed("could not create output path")); }
            return Self::write_file(&out, info.file_bytes());
        }

        let rel = match orig_file.find('\\') { Some(p) => &orig_file[p + 1..], None => &orig_file[..] };
        if has_unsafe_component(rel) {
            return Err(eso_error!("\tWarning: Skipping Granny filename '{}' that is outside the output path!", orig_file));
        }
        let out = format!("{}.gr2", remove_file_extension(&format!("{}Granny{}{}", base_path, SEP, rel)));
        let out_path = remove_filename(&out);
        if !ensure_path_exists(&out_path) { return Err(failed("could not create output path")); }
        Self::write_file(&out, info.file_bytes())
    }

    fn save_sub_file_xv4(&self, e: &MnfFileEntry, base_path: &str, convert_dds: bool, info: &DatSubFileInfo) -> Result<()> {
        let data = info.file_bytes();
        if data.len() <= 12 { return Err(failed("xv4 data too small")); }

        let out = numeric_name(base_path, e.archive_index, e.index, ".dds");
        if !ensure_path_exists(&remove_filename(&out)) { return Err(failed("could not create output path")); }
        Self::write_file(&out, &data[12..])?;

        if convert_dds {
            let _ = convert_dds_to_png(data, &out);
        }
        Ok(())
    }

    pub fn save_sub_file(&self, e: &MnfFileEntry, base_path: &str, convert_dds: bool, file: Option<&mut CFile>, opts: &SaveSubFileOptions) -> Result<()> {
        let mut info = DatSubFileInfo::default();

        if !opts.extract_file_extension.is_empty() {
            match self.zosft_name(e) {
                Some(n) if string_ends_with(n, &opts.extract_file_extension) => {}
                _ => return Err(failed("filtered by extension")),
            }
        }
        if !opts.match_filename.is_empty() {
            match self.zosft_name(e) {
                Some(n) if n.contains(&opts.match_filename) => {}
                _ => return Err(failed("filtered by filename")),
            }
        }

        if self.read_data_file(e, &mut info, file).is_err() {
            // Special case for the first MNF file with known data truncation issues
            if e.archive_index == 0 && e.index >= 880 {
                print_debug!("\tError: Failed to load the file data from MNF {:03} with file index {} (absolute index {})!", e.archive_index, e.true_index(), e.index);
                return Err(eso_error!("\tWarning: Failed to load subfile {} from the end of MNF 000 due to a truncated file (this is a known issue with this file)!", e.index));
            }
            return Err(eso_error!("\tError: Failed to load the file data from MNF {:03} with file index {} (absolute index {})!", e.archive_index, e.true_index(), e.index));
        }

        if !info.has_file_data() {
            return Err(eso_error!("\tError: No uncompressed data to write to file!"));
        }

        let ext = guess_file_extension(info.file_bytes());

        if self.zosft_name(e).map_or(false, |n| !n.is_empty()) {
            let _ = self.save_sub_file_zosft(e, base_path, convert_dds, &info);
        }

        if ext == "gr2" && !opts.no_extract_gr2 {
            let _ = self.save_sub_file_gr2(e, base_path, &info);
        } else if ext == "EsoFileData" {
            let _ = match opts.extract_sub_file_data_type.as_str() {
                "combined" => self.extract_sub_file_data_combined(e, base_path, &info),
                "seperate" => self.extract_sub_file_data_seperate(e, base_path, &info),
                _ => Ok(()),
            };
        } else if ext == "xv4" {
            let _ = self.save_sub_file_xv4(e, base_path, convert_dds, &info);
        }

        let out = numeric_name(base_path, e.archive_index, e.index, &format!(".{}", ext));
        if !ensure_path_exists(&remove_filename(&out)) { return Err(failed("could not create output path")); }
        Self::write_file(&out, info.file_bytes())?;

        if convert_dds && string_ends_with(&out, ".dds") {
            let _ = convert_dds_to_png(info.file_bytes(), &out);
        }

        if ext == "riff" && !opts.no_convert_riff {
            let _ = self.convert_riff_file(e, &out, &info);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // RIFF conversion
    // -----------------------------------------------------------------------------------------

    pub fn convert_riff_file(&self, e: &MnfFileEntry, output_filename: &str, info: &DatSubFileInfo) -> Result<()> {
        let d = info.file_bytes();

        if d.len() > i32::MAX as usize {
            return Err(eso_error!("RIFF filesize of {} bytes is too large to convert!", d.len()));
        }
        if d.len() < 72 {
            return Err(eso_error!("RIFF filesize of {} bytes is too small to convert!", d.len()));
        }
        if &d[0..4] != b"RIFF" {
            return Err(eso_error!("Missing 'RIFF' file header...file is probably not a RIFF file!"));
        }
        if &d[8..12] != b"WAVE" {
            return Err(eso_error!("Unknown RIFF type '{}' found, expecting 'WAVE'!", String::from_utf8_lossy(&d[8..12])));
        }
        if &d[12..16] != b"fmt " {
            return Err(eso_error!("Unknown RIFF chunk '{}' found, expecting 'fmt '!", String::from_utf8_lossy(&d[12..16])));
        }

        match u16::from_le_bytes([d[20], d[21]]) {
            0xFFFF => self.convert_riff_file_to_ogg(e, output_filename, info),
            0xFFFE => self.convert_riff_file_to_wav(e, output_filename, info),
            other => Err(eso_error!("Unknown RIFF format 0x{:04X} found, expecting 0xFFFE or 0xFFFF!", other)),
        }
    }

    /// TODO: needs a port of ww2ogg (`Wwise_RIFF_Vorbis`, src/wwriff.h) and `packed_codebooks_aoTuV_603.bin`
    /// (expected next to the executable: see `ww2ogg_packed_bin_filename`).
    pub fn convert_riff_file_to_ogg(&self, _e: &MnfFileEntry, output_filename: &str, _info: &DatSubFileInfo) -> Result<()> {
        Err(eso_error!("Error: RIFF->OGG conversion (ww2ogg) is not implemented yet; not converting '{}' (codebook file: '{}')!",
            output_filename, self.ww2ogg_packed_bin_filename))
    }

    /// Port of the in-class WAV conversion.  NOTE: the growth of the file (+4 bytes, RIFF size +4)
    /// doesn't match the +16 applied to the fmt/cb sizes and the chunk shift below, so the last 12 bytes of
    /// the data are cut off.  Reproduced as-is; check against real files.
    pub fn convert_riff_file_to_wav(&self, _e: &MnfFileEntry, output_filename: &str, info: &DatSubFileInfo) -> Result<()> {
        let d = info.file_bytes();
        let new_size = d.len() + 4;
        let mut out = vec![0u8; new_size + 100];
        out[..72].copy_from_slice(&d[..72]);

        let riff_size = u32::from_le_bytes(d[4..8].try_into().unwrap()).wrapping_add(4);
        out[4..8].copy_from_slice(&riff_size.to_le_bytes());

        let fmt_size = u32::from_le_bytes(d[16..20].try_into().unwrap()).wrapping_add(0x10);
        out[16..20].copy_from_slice(&fmt_size.to_le_bytes());

        let old_cb = u16::from_le_bytes(d[36..38].try_into().unwrap());
        let cb = old_cb.wrapping_add(0x10);
        out[36..38].copy_from_slice(&cb.to_le_bytes());

        let valid_bits = u16::from_le_bytes(d[38..40].try_into().unwrap()).wrapping_add(0x10);
        out[38..40].copy_from_slice(&valid_bits.to_le_bytes());

        out[44..60].copy_from_slice(b"\x01\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xAA\x00\x38\x9B\x71");

        let old_chunk = 36 + 2 + old_cb as usize;
        let new_chunk = 36 + 2 + cb as usize;
        if old_chunk > d.len() {
            return Err(eso_error!("Invalid cbSize 0x{:04X} in RIFF file!", old_cb));
        }
        let len = d.len() - old_chunk;
        if new_chunk + len > out.len() {
            return Err(eso_error!("RIFF data too large to convert!"));
        }
        out[new_chunk..new_chunk + len].copy_from_slice(&d[old_chunk..]);

        Self::write_file(&format!("{}.wav", output_filename), &out[..new_size])
    }

    // -----------------------------------------------------------------------------------------
    // Bulk export
    // -----------------------------------------------------------------------------------------

    pub fn save_sub_file_by_index(&self, file_index: usize, opts: &MnfExportOptions) -> Result<()> {
        let Some(&i) = self.index_map.get(&(file_index as u32)) else {
            return Err(eso_error!("Failed to find the file with index {} (0x{:08X}) in MNF file!", file_index, file_index));
        };
        print_error!("Saving file index {} (0x{:08X}) from MNF file...", file_index, file_index);
        print_error!("Saving data to '{}'...", opts.output_path);
        self.save_sub_file(&self.file_table[i], &opts.output_path, opts.convert_dds, None, &SaveSubFileOptions::from_export(opts))
    }

    /// Exports several files by (true) file index. The MNF and ZOSFT are already loaded once by `export`;
    /// the files are read in archive/offset order, reusing one open DAT handle per archive.
    pub fn save_sub_files_by_index(&self, indices: &[u32], opts: &MnfExportOptions) -> Result<()> {
        let start_time = get_timer_ms();
        let mut seen = std::collections::HashSet::new();
        let mut found: Vec<(u32, usize)> = Vec::with_capacity(indices.len());
        let mut missing = 0usize;

        for &idx in indices {
            if !seen.insert(idx) { continue; }
            match self.index_map.get(&idx) {
                Some(&i) => found.push((idx, i)),
                None => {
                    missing += 1;
                    print_error!("Failed to find the file with index {} (0x{:08X}) in MNF file!", idx, idx);
                }
            }
        }

        found.sort_by_key(|&(_, i)| (self.file_table[i].archive_index, self.file_table[i].offset));
        print_error!("Saving {} file indices from MNF file to '{}'...", found.len(), opts.output_path);

        let sub_opts = SaveSubFileOptions::from_export(opts);
        let mut input = CFile::new();
        let mut last_archive: Option<u8> = None;
        let mut dat_ok = false;
        let mut success = 0usize;

        for &(idx, i) in &found {
            let entry = self.file_table[i];

            if last_archive != Some(entry.archive_index) {
                last_archive = Some(entry.archive_index);
                input.close();
                let name = self.create_data_filename(entry.archive_index);
                dat_ok = input.open(&name, "rb").is_ok() && input.size() > 14;
                if !dat_ok { print_error!("Error: Failed to open or empty DAT '{}'...", name); }
            }
            if !dat_ok { continue; }

            print_debug!("Saving file index {} (0x{:08X})...", idx, idx);
            if self.save_sub_file(&entry, &opts.output_path, opts.convert_dds, Some(&mut input), &sub_opts).is_ok() {
                success += 1;
            }
        }

        let total = found.len() + missing;
        print_error!("Successfully exported {} of {} files in {:.2} secs!", success, total, (get_timer_ms() - start_time) / 1000.0);
        if success == total { Ok(()) } else { Err(failed("some files could not be exported")) }
    }

    fn sorted_indices(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.file_table.len()).collect();
        v.sort_by_key(|&i| (self.file_table[i].archive_index, self.file_table[i].offset)); // l_FileSortFunc
        v
    }

    pub fn save_sub_files(&self, opts: &MnfExportOptions) -> Result<()> {
        let start_time = get_timer_ms();
        let sorted = self.sorted_indices();
        if sorted.is_empty() {
            return Err(eso_error!("No entries in MNF file to export!"));
        }
        let arch = |i: usize| self.file_table[sorted[i]].archive_index;

        let mut start = 0usize;
        let mut end = sorted.len() - 1;
        let mut archive_count = 0usize;

        if opts.archive_index >= 0 {
            let mut i = 0;
            while i < sorted.len() {
                if arch(i) as i32 == opts.archive_index { start = i; break; }
                i += 1;
            }
            while i < sorted.len() {
                if arch(i) as i32 != opts.archive_index { end = i - 1; break; }
                archive_count += 1;
                i += 1;
            }
            if archive_count == 0 {
                return Err(eso_error!("No files in MNF that have an archive index of {:05}!", opts.archive_index));
            }
        }

        if opts.mnf_start_index > 0 { start = opts.mnf_start_index as usize; }
        if opts.mnf_end_index > 0 { end = opts.mnf_end_index as usize; }
        end = end.min(sorted.len() - 1);

        print_debug!("Sorted MNF file table in {} ms...", get_timer_ms() - start_time);
        let start_time = get_timer_ms();

        if opts.archive_index >= 0 {
            print_error!("Saving {} sub-files ({}-{}) from archive {:04} in MNF file...", archive_count, start, end, opts.archive_index);
        } else if opts.begin_archive_index >= 0 {
            print_error!("Saving sub-files from archive {:04} and above in MNF file...", opts.begin_archive_index);
        } else {
            print_error!("Saving {} sub-files ({}-{}) in MNF file...", (end + 1).saturating_sub(start), start, end);
        }
        print_debug!("Saving sub-files to '{}'...", opts.output_path);

        let sub_opts = SaveSubFileOptions::from_export(opts);
        let mut input_file = CFile::new();
        let mut last_archive = -1i32;
        let mut skip_empty_dat = false;
        let mut save_result = true;
        let mut success = 0usize;

        for i in start..=end {
            if opts.archive_index >= 0 && opts.archive_index != arch(i) as i32 { continue; }
            if opts.begin_archive_index >= 0 && (arch(i) as i32) < opts.begin_archive_index { continue; }

            if i % 100 == 0 && i > 0 {
                if opts.archive_index >= 0 {
                    print_error!("\tSubfile {:7} of {:7}: {:.0}% complete...", i, archive_count, (i - start) as f32 * 100.0 / (archive_count + 1) as f32);
                } else {
                    print_error!("\tSubfile {:7} of {:7}: {:.0}% complete...", i, end - start, (i - start) as f32 * 100.0 / (end - start + 1) as f32);
                }
            }

            if arch(i) as i32 != last_archive {
                skip_empty_dat = false;
                last_archive = arch(i) as i32;
                input_file.close();

                let name = self.create_data_filename(arch(i));
                if input_file.open(&name, "rb").is_err() {
                    print_error!("Error: Failed to open DAT '{}'...", name);
                    skip_empty_dat = true;
                    continue;
                }
                if input_file.size() <= 14 {
                    print_error!("Skipping empty DAT '{}'...", name);
                    skip_empty_dat = true;
                    continue;
                }
                print_error!("Loading DAT '{}'...", name);
            }

            if skip_empty_dat { continue; }

            let entry = self.file_table[sorted[i]];
            let ok = self.save_sub_file(&entry, &opts.output_path, opts.convert_dds, Some(&mut input_file), &sub_opts).is_ok();
            if ok { success += 1; }
            save_result &= ok;
        }

        print_error!("Successfully exported {} sub-files in {} secs!", success, (get_timer_ms() - start_time) / 1000.0);
        if save_result { Ok(()) } else { Err(failed("some sub-files could not be exported")) }
    }

    /// Older bulk exporters (unused by `export`).
    pub fn save_sub_files_basic(&self, base_path: &str, convert_dds: bool, start_index: usize) -> Result<()> {
        let start_time = get_timer_ms();
        let mut ok = true;
        print_error!("Saving {} sub-files referenced in MNF file to '{}'...", self.file_table.len(), base_path);

        for (i, e) in self.file_table.iter().enumerate().skip(start_index) {
            if i % 100 == 0 {
                print_error!("\tSubfile {:7} of {:7}: {:.0}% complete...", i, self.file_table.len(), i as f32 * 100.0 / self.file_table.len() as f32);
            }
            ok &= self.save_sub_file(e, base_path, convert_dds, None, &SaveSubFileOptions::default()).is_ok();
        }
        print_log!("Exported {} sub-files in {} secs!", self.file_table.len(), (get_timer_ms() - start_time) * 1000.0);
        if ok { Ok(()) } else { Err(failed("some sub-files could not be exported")) }
    }

    pub fn save_sub_files_fast(&self, base_path: &str, convert_dds: bool, start_index: usize) -> Result<()> {
        let sorted = self.sorted_indices();
        let (mut input, mut last_archive, mut ok, mut success) = (CFile::new(), -1i32, true, 0usize);
        let start_time = get_timer_ms();

        print_error!("Saving {} sub-files referenced in MNF file to '{}'...", sorted.len(), base_path);

        for (i, &idx) in sorted.iter().enumerate().skip(start_index) {
            let e = self.file_table[idx];
            if i % 100 == 0 {
                print_error!("\tSubfile {:7} of {:7}: {:.0}% complete...", i, sorted.len(), i as f32 * 100.0 / sorted.len() as f32);
            }
            if e.archive_index as i32 != last_archive {
                let name = self.create_data_filename(e.archive_index);
                input.close();
                print_error!("Starting archive {}...", name);
                if input.open(&name, "rb").is_err() { continue; }
                last_archive = e.archive_index as i32;
            }
            let r = self.save_sub_file(&e, base_path, convert_dds, Some(&mut input), &SaveSubFileOptions::default()).is_ok();
            if r { success += 1; }
            ok &= r;
        }
        print_debug!("Successfully exported {} sub-files in {} secs!", success, (get_timer_ms() - start_time) / 1000.0);
        if ok { Ok(()) } else { Err(failed("some sub-files could not be exported")) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{MnfBlockData, MnfBlockHeader};

    fn words(v: &[u32]) -> Vec<u8> { v.iter().flat_map(|w| w.to_le_bytes()).collect() }

    fn rec3(size: u32, csize: u32, hash: u32, offset: u32, ctype: u8, arch: u8) -> Vec<u8> {
        let mut v = words(&[size, csize, hash, offset]);
        v.extend([ctype, arch, 0, 0]);
        v
    }

    fn mnf_with_blocks(version: u16) -> MnfFile {
        let mut m = MnfFile::new();
        m.header.version = version;
        let mut d3 = rec3(100, 50, 0xAAAA, 0x10, 2, 7);
        d3.extend(rec3(200, 80, 0xBBBB, 0x40, 1, 8));
        m.block3.set_contents(
            MnfBlockHeader { record_count23: 2, ..Default::default() },
            vec![
                MnfBlockData::new(words(&[0x8000_0001, 5, 0x8000_0002])),
                MnfBlockData::new(words(&[11, 0, 22, 1])),
                MnfBlockData::new(d3),
            ],
        );
        m
    }

    #[test]
    fn builds_table_and_maps() {
        let mut m = mnf_with_blocks(2);
        m.create_file_table().unwrap();
        let t = m.file_table();
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].id1, t[1].id1), (0x8000_0001, 0x8000_0002));
        assert_eq!((t[0].file_index, t[1].file_index, t[1].unknown1), (11, 22, 1));
        assert_eq!(t[1].true_index(), 23);
        assert_eq!((t[0].compress_type, t[0].archive_index), (2, 7));
        assert_eq!(m.hash_map[&0xBBBB], 1);
        assert_eq!(m.index_map[&23], 1);
    }

    #[test]
    fn version3_swaps_compress_type_and_archive() {
        let mut m = mnf_with_blocks(3);
        m.create_file_table().unwrap();
        assert_eq!((m.file_table()[0].compress_type, m.file_table()[0].archive_index), (7, 2));
    }

    #[test]
    fn zosft_lookup_prefers_index_then_hash() {
        let mut m = mnf_with_blocks(2);
        m.create_file_table().unwrap();
        m.filename = "game.mnf".into();
        assert_eq!(m.get_zosft_file_entry(), None);
        m.index_map.insert(0, 1);
        assert_eq!(m.get_zosft_file_entry(), Some(1));
    }

    #[test]
    fn data_filename() {
        let mut m = MnfFile::new();
        m.filename = "game.mnf".into();
        assert_eq!(m.create_data_filename(7), "game0007.dat");
    }

    #[test]
    fn saves_uncompressed_sub_file_end_to_end() {
        let dir = std::env::temp_dir().join(format!("eso_mnf_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let payload = b"-- hello lua\nprint(1)\n";
        let mut dat = vec![0u8; 16]; // padding before the sub-file
        dat.extend_from_slice(payload);
        std::fs::write(dir.join("game0007.dat"), &dat).unwrap();

        let mut m = MnfFile::new();
        m.filename = dir.join("game.mnf").to_string_lossy().into_owned();
        let entry = MnfFileEntry {
            index: 5, size: payload.len() as u32, compressed_size: payload.len() as u32,
            offset: 16, compress_type: 0, archive_index: 7, ..Default::default()
        };

        let out = format!("{}{}", dir.join("out").to_string_lossy(), SEP);
        m.save_sub_file(&entry, &out, false, None, &SaveSubFileOptions::default()).unwrap();

        let written = std::fs::read(dir.join("out").join("007").join("000005.lua")).unwrap();
        assert_eq!(&written[..], &payload[..]);
        let _ = std::fs::remove_dir_all(&dir);
    }


    fn temp_mnf_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("eso_mnf_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_entry_and_batch_read_in_memory() {
        let dir = temp_mnf_dir("read");
        let payload = b"in-memory payload";
        let mut dat = vec![0u8; 16];
        dat.extend_from_slice(payload);
        std::fs::write(dir.join("game0007.dat"), &dat).unwrap();

        let mut m = MnfFile::new();
        m.filename = dir.join("game.mnf").to_string_lossy().into_owned();
        // Entry 5 is the payload; the table must satisfy file_table()[i].index == i.
        m.file_table = (0..6u32)
            .map(|i| MnfFileEntry { index: i, ..Default::default() })
            .collect();
        m.file_table[5] = MnfFileEntry {
            index: 5, size: payload.len() as u32, compressed_size: payload.len() as u32,
            offset: 16, compress_type: 0, archive_index: 7, ..Default::default()
        };

        let e = m.file_table[5];
        assert_eq!(m.read_entry(&e, None).unwrap(), payload);

        let mut got = Vec::new();
        m.read_entries_each(&[99, 5, 5], |idx, r| got.push((idx, r.ok())));
        assert_eq!(got.len(), 2, "duplicates are reported once, missing ones as errors");
        assert_eq!(got[0].0, 99);
        assert!(got[0].1.is_none());
        assert_eq!(got[1], (5, Some(payload.to_vec())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oodle_entry_without_library_is_an_error_not_garbage() {
        let dir = temp_mnf_dir("oodle");
        // Oodle v3 header (0x8C 0x06) but the library is not loaded in tests.
        let blob = [0x8C, 0x06, 1, 2, 3, 4, 5, 6];
        std::fs::write(dir.join("game0001.dat"), [0u8; 16].iter().chain(blob.iter()).copied().collect::<Vec<u8>>()).unwrap();

        let mut m = MnfFile::new();
        m.header.version = 3;
        m.filename = dir.join("game.mnf").to_string_lossy().into_owned();
        let e = MnfFileEntry { index: 0, size: 8, compressed_size: 8, offset: 16, compress_type: 0, archive_index: 1, ..Default::default() };
        assert!(m.read_entry(&e, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

}
