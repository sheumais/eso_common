//! Port of the sub-file classes `CEsoSubFileDataRecord`, `CEsoSubFileDataFile` and
//! `CEsoSubFileIndexFile` (used for `--extractsubfile`).  All values are big endian.
//!
//! * Data file: 16 byte header (magic `0xFAFAEBEB`, unknown, record count, unknown) followed by
//!   records of `[size1, size2, compressed size]` + zlib data.
//! * Index file: 30 byte header (the `unknown6` field was added in a later game update) followed by
//!   `(index, offset)` pairs, one per data record.
//!
//! Deliberate differences from the C++, all of which only affect corrupt input: bounds checks are
//! relative to the current position, and a record that fails to decompress is kept with no data
//! (instead of exposing an uninitialised buffer), so it is skipped when saving.

use crate::error::Result;
use crate::file::CFile;
use crate::{eso_error, print_debug, print_error};

const DATA_HEADER_SIZE: usize = 16;
const INDEX_HEADER_SIZE: usize = 30;
const RECORD_HEADER_SIZE: usize = 12;

fn be32(d: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(d[off..off + 4].try_into().unwrap())
}

fn be16(d: &[u8], off: usize) -> u16 {
    u16::from_be_bytes(d[off..off + 2].try_into().unwrap())
}

/// `eso_indexsubfile_data_t`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubFileIndexRecord {
    pub index: u32,
    pub offset: u32,
}

// ---------------------------------------------------------------------------------------------
// CEsoSubFileDataRecord
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct EsoSubFileDataRecord {
    pub uncompressed_size1: u32,
    pub uncompressed_size2: u32,
    pub compressed_size: u32,
    pub compressed_data: Vec<u8>,
    pub uncompressed_data: Vec<u8>,
    /// From the index file (`UpdateIndex`).
    pub index: u32,
    pub orig_file_offset: u32,
}

impl EsoSubFileDataRecord {
    pub fn uncompressed_size(&self) -> usize {
        self.uncompressed_data.len()
    }

    pub fn destroy(&mut self) {
        self.compressed_data = Vec::new();
        self.uncompressed_data = Vec::new();
        self.compressed_size = 0;
    }

    /// Parses one record at `*pos` and advances `*pos` past it.
    pub fn parse(&mut self, data: &[u8], pos: &mut usize) -> Result<()> {
        if *pos >= data.len() || data.len() - *pos < RECORD_HEADER_SIZE {
            return Err(eso_error!("\t\tNot enough data for subfile data header!"));
        }
        let p = *pos;
        self.uncompressed_size1 = be32(data, p);
        self.uncompressed_size2 = be32(data, p + 4);
        self.compressed_size = be32(data, p + 8);

        if self.compressed_size as usize > data.len() - p - RECORD_HEADER_SIZE {
            return Err(eso_error!("\t\tNot enough data for uncompressed subfile data!"));
        }
        if self.uncompressed_size1 == 0 || self.compressed_size == 0 {
            return Err(eso_error!("\t\tEmpty compressed file found (skipping rest of empty data)!"));
        }

        let start = p + RECORD_HEADER_SIZE;
        self.compressed_data = data[start..start + self.compressed_size as usize].to_vec();

        let max_output = self.uncompressed_size1 as usize + 16;
        let mut out = vec![0u8; max_output + 1];

        match crate::util::inflate_zlib_block(&mut out[..max_output], &self.compressed_data, false) {
            Ok(n) => {
                out.truncate(n);
                self.uncompressed_data = out;
                if self.uncompressed_size1 != self.uncompressed_size2 {
                    print_error!("\t\tUncompressed stream ({} / {} / {} bytes)",
                        n, self.uncompressed_size1, self.uncompressed_size2);
                }
            }
            Err(_) => {
                self.uncompressed_data = Vec::new();
                print_error!("\t\tError uncompressing stream ({} / {} / {} bytes)!",
                    max_output, self.uncompressed_size1, self.uncompressed_size2);
            }
        }

        *pos = start + self.compressed_size as usize;
        Ok(())
    }

    /// Writes the uncompressed data to its own file.
    pub fn save(&self, filename: &str) -> Result<()> {
        if self.uncompressed_data.is_empty() {
            return Err(eso_error!("\t\tNo uncompressed data to save to file '{}'!", filename));
        }
        let mut f = CFile::new();
        f.open(filename, "wb")?;
        f.write_bytes(&self.uncompressed_data)
    }

    /// Appends the record to a combined file: `"####"`, seven big endian dwords, then the data.
    pub fn save_combined(&self, file: &mut CFile, index: u32) -> Result<()> {
        let mut buf = Vec::new();
        self.append_combined(&mut buf, index)?;
        file.write_bytes(&buf)
    }

    /// The bytes [`save_combined`](Self::save_combined) would write, appended to `out`.
    pub fn append_combined(&self, out: &mut Vec<u8>, index: u32) -> Result<()> {
        if self.uncompressed_data.is_empty() {
            return Err(eso_error!("\t\tNo uncompressed data to save!"));
        }
        self.write_combined_unchecked(out, index);
        Ok(())
    }

    fn write_combined_unchecked(&self, out: &mut Vec<u8>, index: u32) {
        out.extend_from_slice(b"####");
        for v in [
            index,
            self.uncompressed_size1,
            self.uncompressed_size2,
            self.compressed_size,
            self.index,
            self.orig_file_offset,
            self.uncompressed_data.len() as u32,
        ] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&self.uncompressed_data);
    }
}

// ---------------------------------------------------------------------------------------------
// CEsoSubFileDataFile
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct EsoSubFileDataFile {
    pub magic_bytes: u32,
    pub unknown1: u32,
    pub num_records: u32,
    pub unknown2: u32,
    pub records: Vec<EsoSubFileDataRecord>,
}

impl Default for EsoSubFileDataFile {
    fn default() -> Self {
        Self { magic_bytes: 0xFAFA_EBEB, unknown1: 0x11, num_records: 0, unknown2: 0, records: Vec::new() }
    }
}

impl EsoSubFileDataFile {
    pub fn num_sub_files(&self) -> usize {
        self.records.len()
    }

    pub fn destroy(&mut self) {
        self.records.clear();
        self.num_records = 0;
    }

    /// For testing only.
    pub fn load(&mut self, filename: &str) -> Result<()> {
        let data = CFile::read_all_from(filename)?;
        self.parse(&data)
    }

    pub fn parse_header(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < DATA_HEADER_SIZE {
            return Err(eso_error!("\t\tNot enough data for subfile header ({:08X})!", data.len()));
        }
        self.magic_bytes = be32(data, 0);
        self.unknown1 = be32(data, 4);
        self.num_records = be32(data, 8);
        self.unknown2 = be32(data, 12);
        Ok(())
    }

    /// Parses all records.  Records read before an error are kept.
    pub fn parse(&mut self, data: &[u8]) -> Result<()> {
        self.records.clear();
        self.parse_header(data)?;

        let mut pos = DATA_HEADER_SIZE;
        while pos < data.len() {
            let mut rec = EsoSubFileDataRecord::default();
            rec.parse(data, &mut pos)?;
            self.records.push(rec);
        }
        Ok(())
    }

    /// Saves each record's uncompressed data to `<path><n>.dat` (n starting at 1).
    pub fn save_files(&self, path: &str) -> Result<()> {
        print_error!("\t\tSaving uncompressed record data to seperate files in '{}'...", path);

        for (i, rec) in self.records.iter().enumerate() {
            let n = i + 1;
            if n % 1000 == 0 {
                print_error!("\t\tSaved {} of {} files...", n, self.records.len());
            }
            let _ = rec.save(&format!("{}{}.dat", path, n)); // failures are logged; keep going
        }
        Ok(())
    }

    /// The combined layout written by [`save_combined_file`](Self::save_combined_file), in memory:
    /// the 16 byte header (big endian) followed by every record that has data, numbered by its
    /// position in `records` (1-based, counting skipped records too).
    pub fn to_combined_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for v in [self.magic_bytes, self.unknown1, self.num_records, self.unknown2] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        for (i, rec) in self.records.iter().enumerate() {
            if rec.uncompressed_data.is_empty() {
                continue; // record failed to decompress
            }
            rec.write_combined_unchecked(&mut out, i as u32 + 1);
        }
        out
    }

    pub fn save_combined_file(&self, filename: &str) -> Result<()> {
        let mut f = CFile::new();
        f.open(filename, "wb")?;

        print_debug!("\t\tSaving uncompressed record data to combined file '{}'...", filename);

        f.write_bytes(&self.to_combined_bytes())?;
        f.close();
        Ok(())
    }

    /// Copies the index file's (index, offset) pairs onto the matching records.
    pub fn update_index(&mut self, records: &[SubFileIndexRecord]) {
        for (rec, idx) in self.records.iter_mut().zip(records) {
            rec.index = idx.index;
            rec.orig_file_offset = idx.offset;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// CEsoSubFileIndexFile
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct EsoSubFileIndexFile {
    pub magic_bytes: u32,
    pub unknown1: u32,
    pub unknown2: u16,
    pub unknown3: u32,
    pub unknown4: u32,
    pub unknown5: u32,
    /// Added in a game update.
    pub unknown6: u32,
    pub num_records: u32,
    records: Vec<SubFileIndexRecord>,
}

impl Default for EsoSubFileIndexFile {
    fn default() -> Self {
        Self {
            magic_bytes: 0xFBFB_ECEC,
            unknown1: 4,
            unknown2: 1,
            unknown3: 0x11,
            unknown4: 0,
            unknown5: 0,
            unknown6: 0,
            num_records: 0,
            records: Vec::new(),
        }
    }
}

impl EsoSubFileIndexFile {
    pub fn num_records(&self) -> usize {
        self.records.len()
    }

    pub fn records(&self) -> &[SubFileIndexRecord] {
        &self.records
    }

    pub fn records_mut(&mut self) -> &mut Vec<SubFileIndexRecord> {
        &mut self.records
    }

    pub fn destroy(&mut self) {
        self.records.clear();
        self.num_records = 0;
    }

    /// For testing only.
    pub fn load(&mut self, filename: &str) -> Result<()> {
        let data = CFile::read_all_from(filename)?;
        self.parse(&data)
    }

    pub fn parse_header(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < INDEX_HEADER_SIZE {
            return Err(eso_error!("\t\tNot enough data for index subfile header ({:08X})!", data.len()));
        }
        self.magic_bytes = be32(data, 0);
        self.unknown1 = be32(data, 4);
        self.unknown2 = be16(data, 8);
        self.unknown3 = be32(data, 10);
        self.unknown4 = be32(data, 14);
        self.unknown5 = be32(data, 18);
        self.unknown6 = be32(data, 22);
        self.num_records = be32(data, 26);

        print_debug!("\t\tLoading subfile index: 0x{:08X}, 0x{:08X}, {} records, 0x{:08X}",
            self.magic_bytes, self.unknown1, self.num_records, self.unknown2);
        Ok(())
    }

    pub fn parse(&mut self, data: &[u8]) -> Result<()> {
        self.records.clear();
        self.parse_header(data)?;

        // Don't trust num_records for the allocation, it comes from the file.
        self.records.reserve(((data.len() - INDEX_HEADER_SIZE) / 8).min(self.num_records as usize));

        let mut pos = INDEX_HEADER_SIZE;
        while pos + 8 <= data.len() {
            self.records.push(SubFileIndexRecord { index: be32(data, pos), offset: be32(data, pos + 4) });
            pos += 8;
        }

        if pos < data.len() {
            print_debug!("\t\tWarning: {} extra bytes left over at end of index subfile!", data.len() - pos);
        }
        if self.num_records as usize != self.records.len() {
            print_debug!("\t\tWarning: Record count mismatch in index subfile ({} / {})!", self.num_records, self.records.len());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::deflate_zlib_block;

    fn be(v: u32) -> [u8; 4] { v.to_be_bytes() }

    fn record(raw: &[u8]) -> Vec<u8> {
        let mut c = vec![0u8; raw.len() + 64];
        let n = deflate_zlib_block(&mut c, raw, true).unwrap();
        let mut v = Vec::new();
        v.extend(be(raw.len() as u32)); v.extend(be(raw.len() as u32)); v.extend(be(n as u32));
        v.extend(&c[..n]);
        v
    }

    fn data_file(recs: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(be(0xFAFAEBEB)); v.extend(be(0x11)); v.extend(be(recs.len() as u32)); v.extend(be(0));
        for r in recs { v.extend(record(r)); }
        v
    }

    fn index_file(pairs: &[(u32, u32)]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(be(0xFBFBECEC)); v.extend(be(4)); v.extend(1u16.to_be_bytes());
        v.extend(be(0x11)); v.extend(be(0)); v.extend(be(0)); v.extend(be(0));
        v.extend(be(pairs.len() as u32));
        for (i, o) in pairs { v.extend(be(*i)); v.extend(be(*o)); }
        v
    }

    #[test]
    fn parses_data_and_index_and_roundtrips_combined() {
        let mut d = EsoSubFileDataFile::default();
        d.parse(&data_file(&[b"hello hello hello", b"second record"])).unwrap();
        assert_eq!(d.num_sub_files(), 2);
        assert_eq!(d.records[1].uncompressed_data, b"second record");

        let mut i = EsoSubFileIndexFile::default();
        i.parse(&index_file(&[(7, 100), (8, 200)])).unwrap();
        assert_eq!(i.num_records(), 2);
        d.update_index(i.records());
        assert_eq!((d.records[1].index, d.records[1].orig_file_offset), (8, 200));

        let path = std::env::temp_dir().join("eso_subfile_combined_test.bin");
        let p = path.to_str().unwrap();
        d.save_combined_file(p).unwrap();
        let out = std::fs::read(p).unwrap();
        assert_eq!(&out[..4], &be(0xFAFAEBEB));
        assert_eq!(&out[16..20], b"####");
        assert_eq!(&out[20..24], &be(1)); // 1-based record number
        assert_eq!(&out[36..40], &be(7)); // index from the index file
        assert_eq!(&out[40..44], &be(100)); // original offset
        assert_eq!(&out[44..48], &be(17)); // uncompressed size
        assert_eq!(&out[48..65], b"hello hello hello");
    }

    #[test]
    fn index_header_is_30_bytes() {
        let mut i = EsoSubFileIndexFile::default();
        assert!(i.parse(&[0u8; 29]).is_err());
        i.parse(&index_file(&[])).unwrap();
        assert_eq!(i.num_records(), 0);
    }

    #[test]
    fn corrupt_records_do_not_panic() {
        let mut d = EsoSubFileDataFile::default();
        let mut f = data_file(&[b"abc"]);
        f.extend(be(5)); f.extend(be(5)); f.extend(be(9999)); // compressed size past end
        assert!(d.parse(&f).is_err());
        assert_eq!(d.num_sub_files(), 1); // first record is kept

        let mut z = data_file(&[]);
        z.extend([0u8; 12]); // trailing empty record
        assert!(EsoSubFileDataFile::default().parse(&z).is_err());
        assert!(EsoSubFileDataFile::default().parse(&[0u8; 8]).is_err());
    }


    #[test]
    fn to_combined_bytes_matches_the_file_writer() {
        let mut d = EsoSubFileDataFile::default();
        // An empty middle record must be skipped but still consume its 1-based number.
        d.parse(&data_file(&[b"first", b"third record"])).unwrap();
        d.records.insert(1, EsoSubFileDataRecord::default());
        d.num_records = 3;

        let path = std::env::temp_dir().join(format!("eso_subfile_mem_{}.bin", std::process::id()));
        let p = path.to_str().unwrap();
        d.save_combined_file(p).unwrap();
        let on_disk = std::fs::read(p).unwrap();
        let _ = std::fs::remove_file(&path);

        let in_memory = d.to_combined_bytes();
        assert_eq!(in_memory, on_disk);
        assert_eq!(&in_memory[20..24], &be(1));
        assert!(in_memory.ends_with(b"third record"));
        // header (16) + record 1 ("####" + 28 + 5 bytes) + record 3 ("####" + 28 + 12 bytes)
        assert_eq!(in_memory.len(), 16 + (32 + 5) + (32 + 12));
    }

}
