//! Port of EsoCsvFile.cpp (`CCsvFile`).
//!
//! The parser is a straight port of the original state machine, including its quirks: a backslash
//! inside quotes protects the next quote character, and quotes are *not* required to be doubled.

use crate::cprintf;
use crate::error::Result;
use crate::file::CFile;
use crate::util;

pub type CsvCell = String;
pub type CsvRow = Vec<CsvCell>;

#[derive(Debug, Clone, Default)]
pub struct CsvFile {
    data: Vec<CsvRow>,
    has_header_row: bool,
}

impl CsvFile {
    pub fn new(has_header_row: bool) -> Self {
        Self { data: Vec::new(), has_header_row }
    }

    pub fn data(&self) -> &[CsvRow] { &self.data }
    pub fn has_header_row(&self) -> bool { self.has_header_row }
    pub fn num_rows(&self) -> usize { self.data.len() }
    pub fn num_data_rows(&self) -> usize { self.data.len() - usize::from(self.has_header_row && !self.data.is_empty()) }

    pub fn dump(&self) {
        println!("CSV has {} rows...", self.data.len());
        for (i, row) in self.data.iter().enumerate() {
            print!("{}: ", i);
            for cell in row {
                print!("{}, ", cell);
            }
            println!();
        }
    }

    pub fn load(&mut self, filename: &str) -> Result<()> {
        self.data.clear();
        let bytes = CFile::read_all_from(filename)?;
        self.parse(&bytes);
        Ok(())
    }

    /// Parses CSV data.  The end of the slice acts as the NUL terminator of the C++ version.
    pub fn parse(&mut self, data: &[u8]) {
        // The C++ parser worked on a NUL terminated string: the first NUL ends the data.
        let data = &data[..data.iter().position(|&b| b == 0).unwrap_or(data.len())];
        let at = |i: usize| data.get(i).copied().unwrap_or(0);

        let mut start = 0usize;
        let mut end: Option<usize> = None;
        let mut p = 0usize;
        let mut save_cell = false;
        let mut new_row = true;
        let mut last_quote = false;
        let mut quote_char = 0u8;
        let mut cur_row: Option<usize> = None;

        loop {
            match at(p) {
                b'\\' => {
                    if quote_char != 0 { last_quote = true; }
                }
                b'\r' => {
                    if quote_char == 0 && end.is_none() { end = Some(p); }
                    last_quote = false;
                }
                b'\n' => {
                    if quote_char == 0 {
                        save_cell = true;
                        new_row = true;
                        if end.is_none() { end = Some(p); }
                    }
                    last_quote = false;
                }
                b',' => {
                    if quote_char == 0 {
                        save_cell = true;
                        if end.is_none() { end = Some(p); }
                    }
                    last_quote = false;
                }
                b'"' => {
                    if quote_char != 0 && at(p + 1) == b'"' && !last_quote {
                        p += 1;
                    } else if quote_char != 0 && !last_quote {
                        quote_char = 0;
                        end = Some(p);
                    } else if quote_char == 0 {
                        quote_char = b'"';
                        start = p + 1;
                    }
                    last_quote = false;
                }
                0 => {
                    if end.is_none() { end = Some(p); }
                    save_cell = true;
                    last_quote = false;
                }
                _ => last_quote = false,
            }

            if save_cell {
                let e = end.unwrap_or(p);
                if e <= start && at(p) == 0 {
                    break;
                }

                let row = *cur_row.get_or_insert_with(|| {
                    self.data.push(CsvRow::new());
                    self.data.len() - 1
                });

                let cell = if e <= start { String::new() } else { String::from_utf8_lossy(&data[start..e.min(data.len())]).into_owned() };
                self.data[row].push(cell);

                start = p + 1;
                end = None;
                save_cell = false;
            }

            if new_row {
                cur_row = None;
                new_row = false;
            }

            if p >= data.len() { break; }
            p += 1;
        }
    }

    /// Writes every cell quoted, without escaping (as the original did).
    pub fn save(&self, filename: &str) -> Result<()> {
        let mut f = CFile::new();
        f.open(filename, "wb")?;
        for row in &self.data {
            for (i, cell) in row.iter().enumerate() {
                if i > 0 { f.write_char(b',')?; }
                if !cprintf!(f, "\"{}\"", cell) { return Err(crate::error::EsoError::new("write failed")); }
            }
            f.write_char(b'\n')?;
        }
        Ok(())
    }

    pub fn replace_strings(&mut self, src: &str, dest: &str) {
        for row in &mut self.data {
            for cell in row {
                *cell = util::replace_strings(cell, src, dest);
            }
        }
    }

    pub fn create_new_remove_even_lines(&self) -> CsvFile {
        CsvFile {
            has_header_row: self.has_header_row,
            data: self.data.iter().step_by(2).cloned().collect(),
        }
    }
}
