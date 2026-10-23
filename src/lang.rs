//! Port of EsoLangFile.cpp (`CEsoLangFile`): reading/writing the binary `.lang` format and
//! converting it to and from CSV / PO-CSV / plain text.
//!
//! Binary layout (all values big endian):
//! `u32 file_id (2) | u32 record_count | record_count * { u32 id, u32 unknown, u32 index, u32 offset } | NUL-terminated texts`
//! where `offset` is relative to the start of the text block.
//!
//! Assumptions about code that wasn't supplied (EsoLangFile.h): `TEXT_RECORD_SIZE == 16`, and records
//! sort by (id, unknown, index).

use crate::csv::CsvFile;
use crate::error::Result;
use crate::file::CFile;
use crate::util::{atoi, parse_buffer_dword, parse_buffer_string, parse_id_triplet, replace_strings};
use crate::{cprintf, eso_error, print_error, print_log};
use std::cmp::Ordering;
use std::collections::HashSet;

pub const TEXT_RECORD_SIZE: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LangRecord {
    pub id: u32,
    pub unknown: u32,
    pub index: u32,
    pub offset: u32,
    pub text: String,
}

impl PartialOrd for LangRecord {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl Ord for LangRecord {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.id, self.unknown, self.index).cmp(&(o.id, o.unknown, o.index))
    }
}

/// Reverses the CSV/text escaping: `\n`, `\r` and `""`.
pub fn unescape_lang_text(s: &str) -> String {
    replace_strings(&replace_strings(&replace_strings(s, "\\n", "\n"), "\\r", "\r"), "\"\"", "\"")
}

/// Writes `text` with `\r` / `\n` escaped (and `"` doubled if `escape_quotes`).  Stops at a NUL like the C++ loops.
pub fn output_lang_text(file: &mut CFile, text: &str, escape_quotes: bool) -> bool {
    let mut ok = true;
    for b in text.bytes() {
        if b == 0 { break; }
        ok &= match b {
            b'"' if escape_quotes => file.write_string("\"\"").is_ok(),
            b'\r' => file.write_string("\\r").is_ok(),
            b'\n' => file.write_string("\\n").is_ok(),
            _ => file.write_char(b).is_ok(),
        };
    }
    ok
}

#[derive(Debug, Clone)]
pub struct EsoLangFile {
    file_id: u32,
    records: Vec<LangRecord>,
}

impl Default for EsoLangFile {
    fn default() -> Self { Self { file_id: 0x02, records: Vec::new() } }
}

impl EsoLangFile {
    pub fn new() -> Self { Self::default() }

    pub fn destroy(&mut self) {
        self.records.clear();
        self.file_id = 0x02;
    }

    pub fn num_records(&self) -> usize { self.records.len() }
    pub fn record(&self, i: usize) -> &LangRecord { &self.records[i] }
    pub fn record_mut(&mut self, i: usize) -> &mut LangRecord { &mut self.records[i] }
    pub fn records(&self) -> &[LangRecord] { &self.records }

    /// Adds an entry from a packed id: `id | unknown << 32 | index << 42`.
    pub fn add_entry64(&mut self, id64: u64, text: &str) {
        let id = (id64 & 0xffff_ffff) as u32;
        let unknown = ((id64 >> 32) & 0x3ff) as u32;
        let index = ((id64 >> 42) & 0xfffff) as u32;
        self.add_entry(id, unknown, index, 0, text);
    }

    pub fn add_entry(&mut self, id: u32, unknown: u32, index: u32, offset: u32, text: &str) {
        self.records.push(LangRecord { id, unknown, index, offset, text: text.to_string() });
    }

    pub fn sort_records(&mut self) {
        self.records.sort();
    }

    // -----------------------------------------------------------------------------------------
    // Creating records from CSV / text
    // -----------------------------------------------------------------------------------------

    fn create_records_from_csv(&mut self, csv: &CsvFile, _use_po_source_text: bool) -> usize {
        let mut count = 0;
        for (row_num, r) in csv.data().iter().enumerate() {
            if row_num == 0 && csv.has_header_row() { continue; }
            if r.len() < 5 {
                print_error!("{}: Need at least 5 columns in CSV row to convert to LANG data!", row_num + 1);
                continue;
            }
            self.records.push(LangRecord {
                id: atoi(&r[0]) as u32,
                unknown: atoi(&r[1]) as u32,
                index: atoi(&r[2]) as u32,
                offset: atoi(&r[3]) as u32,
                text: unescape_lang_text(&r[4]),
            });
            count += 1;
        }
        count
    }

    fn create_records_from_po_csv(&mut self, csv: &CsvFile, use_po_source_text: bool) -> usize {
        let mut count = 0;
        let mut offset = 0u32;
        for (row_num, r) in csv.data().iter().enumerate() {
            if row_num == 0 && csv.has_header_row() { continue; }
            if r.len() < 3 {
                print_error!("{}: Need at least 3 columns in a PO-CSV row to convert to LANG data!", row_num + 1);
                continue;
            }
            let mut rec = LangRecord::default();
            match parse_id_triplet(&r[0]) {
                Some((id, unk, idx)) => { rec.id = id; rec.unknown = unk; rec.index = idx; }
                None => print_error!("{}: Failed to convert column 1 value '{}' to ID/Unknown/Index values!", row_num + 1, r[0]),
            }
            rec.offset = offset;
            rec.text = unescape_lang_text(if use_po_source_text { &r[1] } else { &r[2] });
            offset += rec.text.len() as u32 + 1;
            self.records.push(rec);
            count += 1;
        }
        count
    }

    fn create_records_from_text(&mut self, text_file: &[String], id_file: &[String], use_po_format: bool, _use_po_source_text: bool) -> usize {
        let delta = if use_po_format { 2 } else { 1 };
        let expected = if use_po_format { text_file.len() / 2 } else { text_file.len() };
        if id_file.len() != expected {
            print_error!("Warning: Text file and ID file don't have the same number of rows ({} != {})!", expected, id_file.len());
        }

        let mut offset = 0u32;
        let mut count = 0;
        let mut i = 0;
        let mut id = 0;
        while i < text_file.len() && id < id_file.len() {
            let mut rec = LangRecord::default();
            match parse_id_triplet(&id_file[id]) {
                Some((a, b, c)) => { rec.id = a; rec.unknown = b; rec.index = c; }
                None => print_error!("{}: Failed to convert column 1 value '{}' to ID/Unknown/Index values!", count + 1, id_file[id]),
            }
            rec.offset = offset;
            rec.text = unescape_lang_text(&text_file[i]);
            offset += rec.text.len() as u32 + 1;
            self.records.push(rec);
            count += 1;
            id += 1;
            i += delta;
        }
        count
    }

    pub fn create_from_text(&mut self, text_file: &[String], id_file: &[String], use_po_format: bool, use_po_source_text: bool) -> Result<()> {
        self.destroy();
        print_error!("UsePOFormat = {}", use_po_format as i32);

        let expected = if use_po_format { text_file.len() / 2 } else { text_file.len() };
        let got = self.create_records_from_text(text_file, id_file, use_po_format, use_po_source_text);

        if expected != got {
            print_error!("Warning: Expected {} language records from TEXT but only found {}!", expected, got);
        } else {
            print_error!("Created {} language records from TEXT data...", got);
        }
        Ok(())
    }

    pub fn create_from_csv(&mut self, csv: &CsvFile, use_po_format: bool, use_po_source_text: bool) -> Result<()> {
        self.destroy();
        let expected = csv.num_data_rows();
        let got = if use_po_format {
            self.create_records_from_po_csv(csv, use_po_source_text)
        } else {
            self.create_records_from_csv(csv, use_po_source_text)
        };

        if expected != got {
            print_error!("Warning: Expected {} language records from CSV but only found {}!", expected, got);
        } else {
            print_error!("Created {} language records from CSV data...", got);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Dumping
    // -----------------------------------------------------------------------------------------

    /// NOTE: ids are printed with `%d` semantics (as signed 32-bit) like the original, so ids
    /// above `i32::MAX` appear negative.  `atoi` on import wraps back, so it round-trips.
    pub fn dump_csv(&self, filename: &str, use_po_format: bool) -> Result<()> {
        let mut f = CFile::new();
        f.open(filename, "wb")?;

        if use_po_format {
            cprintf!(f, "\"Location\",\"Source\",\"Target\"\n");
        } else {
            cprintf!(f, "\"ID\",\"Unknown\",\"Index\",\"Offset\",\"Text\"\n");
        }

        for r in &self.records {
            if use_po_format {
                cprintf!(f, "\"{}-{}-{}\",\"", r.id as i32, r.unknown as i32, r.index as i32);
                output_lang_text(&mut f, &r.text, true);
                cprintf!(f, "\",\"\"\n");
            } else {
                cprintf!(f, "\"{}\",\"{}\",\"{}\",\"{}\",\"", r.id as i32, r.unknown as i32, r.index as i32, r.offset as i32);
                output_lang_text(&mut f, &r.text, true);
                cprintf!(f, "\"\n");
            }
        }
        Ok(())
    }

    pub fn dump_text(&self, filename: &str, use_po_format: bool) -> Result<()> {
        let mut f = CFile::new();
        f.open(filename, "wb")?;
        for r in &self.records {
            output_lang_text(&mut f, &r.text, false);
            f.write_char(b'\n')?;
            if use_po_format { f.write_char(b'\n')?; }
        }
        Ok(())
    }

    pub fn dump_text_id(&self, filename: &str) -> Result<()> {
        let mut f = CFile::new();
        f.open(filename, "wb")?;
        for r in &self.records {
            cprintf!(f, "{}-{}-{}\n", r.id, r.unknown, r.index);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Merging
    // -----------------------------------------------------------------------------------------

    /// Adds every entry of `base` whose (id, unknown, index) isn't already present.  Returns the number added.
    pub fn fill_missing_entries(&mut self, base: &EsoLangFile) -> usize {
        let index: HashSet<(u32, u32, u32)> = self.records.iter().map(|r| (r.id, r.unknown, r.index)).collect();

        print_error!("\tCreated index with {} entries from a total of {}!", index.len(), self.records.len());
        print_error!("\tMerging in a possible {} entries...", base.records.len());

        let mut filled = 0;
        for r in &base.records {
            // Like the original, only the entries that existed before the merge are checked.
            if !index.contains(&(r.id, r.unknown, r.index)) {
                self.records.push(r.clone());
                filled += 1;
            }
        }
        filled
    }

    // -----------------------------------------------------------------------------------------
    // Binary I/O
    // -----------------------------------------------------------------------------------------

    pub fn load(&mut self, filename: &str) -> Result<()> {
        let mut f = CFile::new();
        f.open(filename, "rb")?;
        self.read(&mut f)
    }

    pub fn read(&mut self, file: &mut CFile) -> Result<()> {
        let data = file.read_all()?;
        self.parse_data(&data)
    }

    pub fn parse_data(&mut self, data: &[u8]) -> Result<()> {
        let size = data.len();
        if size == 0 { return Err(eso_error!("Error: Invalid input received to CEsoLangFile::ParseFile()!")); }
        if size < 8 { return Err(eso_error!("Error: Language file too small (< 8 bytes)!")); }
        if size > i32::MAX as usize { return Err(eso_error!("Error: Language file too big!")); }

        self.file_id = parse_buffer_dword(&data[0..], true);
        let count = parse_buffer_dword(&data[4..], true) as usize;

        if count > (u32::MAX as usize) / TEXT_RECORD_SIZE - 100 {
            return Err(eso_error!("Error: Too many records found in language file ({})!", count));
        }
        if 8 + count * TEXT_RECORD_SIZE > size {
            return Err(eso_error!("Error: Language file has too many records (read past end of file)!"));
        }

        self.records.clear();
        self.records.reserve(count);
        let text_start = count * TEXT_RECORD_SIZE + 8;

        for i in 0..count {
            let o = 8 + i * TEXT_RECORD_SIZE;
            let mut rec = LangRecord {
                id: parse_buffer_dword(&data[o..], true),
                unknown: parse_buffer_dword(&data[o + 4..], true),
                index: parse_buffer_dword(&data[o + 8..], true),
                offset: parse_buffer_dword(&data[o + 12..], true),
                text: String::new(),
            };
            let text_offset = rec.offset as usize + text_start;
            if text_offset < size {
                rec.text = parse_buffer_string(data, text_offset, size);
            } else {
                print_log!("Warning: Read past end of file (offset 0x{:08X}) in text record #{}", text_offset, i);
            }
            self.records.push(rec);
        }
        Ok(())
    }

    pub fn save(&mut self, filename: &str) -> Result<()> {
        let mut f = CFile::new();
        f.open(filename, "wb")?;
        self.write(&mut f)
    }

    /// Writes the binary format; also recomputes every record's `offset`.
    pub fn write(&mut self, file: &mut CFile) -> Result<()> {
        file.write_u32(self.file_id, false)?;
        file.write_u32(self.records.len() as u32, false)?;

        let mut offset = 0u32;
        for r in &mut self.records {
            r.offset = offset;
            file.write_u32(r.id, false)?;
            file.write_u32(r.unknown, false)?;
            file.write_u32(r.index, false)?;
            file.write_u32(r.offset, false)?;
            offset += r.text.len() as u32 + 1;
        }
        for r in &self.records {
            file.write_string(&r.text)?;
            file.write_char(0)?;
        }
        Ok(())
    }
}

