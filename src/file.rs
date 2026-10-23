//! Port of EsoFile.cpp: `CBaseFile` (trait [`BaseFile`]), `CFile` ([`CFile`]) and `CMemoryFile` ([`MemoryFile`]).

use crate::error::{EsoError, Result};
use crate::{eso_debug_error, eso_error, print_log};
use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekType {
    Set,
    Cur,
    End,
}

/// Common interface for reading binary data from a file or memory buffer.
/// `little_endian` follows the C++ flag: `false` means the data is big endian and is swapped.
pub trait BaseFile {
    fn size(&mut self) -> u64;
    fn is_eof(&self) -> bool;
    fn read_bytes(&mut self, out: &mut [u8]) -> Result<()>;
    fn seek(&mut self, pos: i64, kind: SeekType) -> Result<()>;
    fn tell(&mut self) -> u64;

    /// `Seek(pos)` with the C++ default of `SEEK_SET`.
    fn seek_to(&mut self, pos: u64) -> Result<()> {
        self.seek(pos as i64, SeekType::Set)
    }
    fn read_u8(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.read_bytes(&mut b)?;
        Ok(b[0])
    }
    fn read_u16(&mut self, little_endian: bool) -> Result<u16> {
        let mut b = [0u8; 2];
        self.read_bytes(&mut b)?;
        Ok(if little_endian { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }
    fn read_u32(&mut self, little_endian: bool) -> Result<u32> {
        let mut b = [0u8; 4];
        self.read_bytes(&mut b)?;
        Ok(if little_endian { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }
    fn read_u64(&mut self, little_endian: bool) -> Result<u64> {
        let mut b = [0u8; 8];
        self.read_bytes(&mut b)?;
        Ok(if little_endian { u64::from_le_bytes(b) } else { u64::from_be_bytes(b) })
    }
    fn read_f32(&mut self, little_endian: bool) -> Result<f32> {
        let mut b = [0u8; 4];
        self.read_bytes(&mut b)?;
        Ok(if little_endian { f32::from_le_bytes(b) } else { f32::from_be_bytes(b) })
    }
}

// ---------------------------------------------------------------------------------------------
// CFile
// ---------------------------------------------------------------------------------------------

enum Inner {
    Closed,
    Read(BufReader<File>),
    Write(BufWriter<File>),
}

/// A file opened in either read ("rb") or write ("wb" / "ab") mode.
pub struct CFile {
    inner: Inner,
    eof: bool,
}

impl Default for CFile {
    fn default() -> Self {
        Self { inner: Inner::Closed, eof: false }
    }
}

impl CFile {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens `filename` using a C `fopen` style mode string (`"rb"`, `"wb"`, `"ab"`).
    pub fn open(&mut self, filename: &str, mode: &str) -> Result<()> {
        self.close();
        print_log!("Opening file '{}' in mode '{}'...", filename, mode);

        let plus = mode.contains('+');
        let res = if mode.contains('w') {
            File::create(filename).map(|f| Inner::Write(BufWriter::new(f)))
        } else if mode.contains('a') {
            OpenOptions::new().append(true).create(true).open(filename).map(|f| Inner::Write(BufWriter::new(f)))
        } else if mode.contains('r') && !plus {
            File::open(filename).map(|f| Inner::Read(BufReader::new(f)))
        } else {
            return Err(eso_debug_error!("Error: Unsupported file mode '{}' for file '{}'!", mode, filename));
        };

        match res {
            Ok(inner) => {
                self.inner = inner;
                self.eof = false;
                Ok(())
            }
            Err(_) => Err(eso_debug_error!("Error: Failed to open file '{}' in mode '{}'!", filename, mode)),
        }
    }

    pub fn close(&mut self) {
        if let Inner::Write(w) = &mut self.inner {
            let _ = w.flush();
        }
        self.inner = Inner::Closed;
    }

    pub fn is_open(&self) -> bool {
        !matches!(self.inner, Inner::Closed)
    }

    /// `printf`-style output; use with `format_args!` or the [`cprintf!`] macro.
    pub fn printf(&mut self, args: Arguments<'_>) -> bool {
        self.write_fmt(args).is_ok()
    }

    pub fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        if !self.is_open() {
            return Err(eso_debug_error!("Error: No file defined to write to!"));
        }
        self.write_all(data)
            .map_err(|_| eso_debug_error!("Error: Failed to write {} bytes to file!", data.len()))
    }

    pub fn write_char(&mut self, c: u8) -> Result<()> {
        self.write_bytes(&[c])
    }

    /// `little_endian == false` writes the value big endian (matches the C++ flag).
    pub fn write_u32(&mut self, value: u32, little_endian: bool) -> Result<()> {
        let b = if little_endian { value.to_le_bytes() } else { value.to_be_bytes() };
        self.write_bytes(&b)
    }

    pub fn write_string(&mut self, data: &str) -> Result<()> {
        self.write_bytes(data.as_bytes())
    }

    /// Reads a whole file into memory.  Fails for empty files and files of 2GB or more.
    pub fn read_all_from(filename: &str) -> Result<Vec<u8>> {
        let mut f = CFile::new();
        f.open(filename, "rb")?;
        f.read_all()
    }

    pub fn read_all(&mut self) -> Result<Vec<u8>> {
        if !self.is_open() {
            return Err(eso_debug_error!("Error: No file defined to read from!"));
        }
        self.seek(0, SeekType::Set)?;
        let size = self.size();
        if size == 0 {
            return Err(EsoError::new("Error: File is empty!"));
        }
        if size >= i32::MAX as u64 {
            return Err(eso_debug_error!("Error: Exceeded maximum size of file that can be loaded at once!"));
        }
        let mut buf = vec![0u8; size as usize];
        self.read_bytes(&mut buf)?;
        Ok(buf)
    }
}

impl Drop for CFile {
    fn drop(&mut self) {
        self.close();
    }
}

impl Read for CFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.inner {
            Inner::Read(r) => r.read(buf),
            _ => Err(io::Error::new(io::ErrorKind::Other, "file not open for reading")),
        }
    }
}

impl Write for CFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.inner {
            Inner::Write(w) => w.write(buf),
            _ => Err(io::Error::new(io::ErrorKind::Other, "file not open for writing")),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match &mut self.inner {
            Inner::Write(w) => w.flush(),
            _ => Ok(()),
        }
    }
}

impl CFile {
    fn raw_seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match &mut self.inner {
            Inner::Read(r) => r.seek(pos),
            Inner::Write(w) => w.seek(pos),
            Inner::Closed => Err(io::Error::new(io::ErrorKind::Other, "file not open")),
        }
    }
}

impl BaseFile for CFile {
    fn size(&mut self) -> u64 {
        match &mut self.inner {
            Inner::Read(r) => r.get_ref().metadata().map(|m| m.len()).unwrap_or(0),
            Inner::Write(w) => {
                let _ = w.flush();
                w.get_ref().metadata().map(|m| m.len()).unwrap_or(0)
            }
            Inner::Closed => {
                let _ = eso_debug_error!("Error: File not open in CFile::GetSize()!");
                0
            }
        }
    }

    fn is_eof(&self) -> bool {
        !self.is_open() || self.eof
    }

    fn read_bytes(&mut self, out: &mut [u8]) -> Result<()> {
        if !self.is_open() {
            return Err(eso_debug_error!("Error: No file defined to read from!"));
        }
        let mut n = 0;
        while n < out.len() {
            match self.read(&mut out[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if n != out.len() {
            self.eof = true;
            let pos = self.tell();
            return Err(eso_debug_error!("Error: Only read {} of {} bytes from position 0x{:X}!", n, out.len(), pos));
        }
        Ok(())
    }

    fn seek(&mut self, pos: i64, kind: SeekType) -> Result<()> {
        if !self.is_open() {
            return Err(EsoError::new("file not open"));
        }
        let from = match kind {
            SeekType::Set if pos >= 0 => SeekFrom::Start(pos as u64),
            SeekType::Set => return Err(eso_debug_error!("Error: Failed to seek to position 0x{:016X} from start of file!", pos)),
            SeekType::Cur => SeekFrom::Current(pos),
            SeekType::End => SeekFrom::End(pos),
        };
        self.raw_seek(from).map_err(|_| match kind {
            SeekType::Set => eso_debug_error!("Error: Failed to seek to position 0x{:016X} from start of file!", pos),
            SeekType::End => eso_debug_error!("Error: Failed to seek to position 0x{:016X} from end of file!", pos),
            SeekType::Cur => eso_debug_error!("Error: Failed to seek to position 0x{:016X} relative to current file position!", pos),
        })?;
        self.eof = false;
        Ok(())
    }

    fn tell(&mut self) -> u64 {
        self.raw_seek(SeekFrom::Current(0)).unwrap_or_else(|_| {
            let _ = eso_debug_error!("Error: Failed to get current file position!");
            0
        })
    }
}

/// `printf`-style helper: `cprintf!(file, "\"{}\"\n", x)`.
#[macro_export]
macro_rules! cprintf {
    ($f:expr, $($arg:tt)*) => { $f.printf(format_args!($($arg)*)) };
}

// ---------------------------------------------------------------------------------------------
// CMemoryFile
// ---------------------------------------------------------------------------------------------

/// Reads from an in-memory buffer.
///
/// Seek semantics are kept from the C++ class: `SeekType::End` positions relative to the *end*
/// (`pos` bytes before the end), and seeking exactly to the buffer size is an error.
#[derive(Default)]
pub struct MemoryFile {
    buf: Vec<u8>,
    pos: usize,
}

impl MemoryFile {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_vec(buf: Vec<u8>) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn set_buffer(&mut self, buf: Vec<u8>) {
        self.buf = buf;
        self.pos = 0;
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn position(&self) -> usize {
        self.pos
    }
}

impl BaseFile for MemoryFile {
    fn size(&mut self) -> u64 {
        self.buf.len() as u64
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn read_bytes(&mut self, out: &mut [u8]) -> Result<()> {
        if self.pos + out.len() > self.buf.len() {
            let at = self.pos;
            self.pos = self.buf.len();
            return Err(eso_error!("Error: Buffer overflow reading {} bytes at offset 0x{:08X}!", out.len(), at));
        }
        out.copy_from_slice(&self.buf[self.pos..self.pos + out.len()]);
        self.pos += out.len();
        Ok(())
    }

    fn seek(&mut self, pos: i64, kind: SeekType) -> Result<()> {
        let len = self.buf.len();
        match kind {
            SeekType::Set | SeekType::End => {
                if pos < 0 || pos as u64 >= u32::MAX as u64 {
                    return Err(eso_error!("Error: Invalid buffer position 0x{:016X} received in CMemoryFile::Seek()!", pos));
                }
                let p = pos as usize;
                if p >= len {
                    self.pos = if kind == SeekType::Set { len } else { 0 };
                    return Err(eso_error!("Error: Position 0x{:016X} exceeds size of buffer!", pos));
                }
                self.pos = if kind == SeekType::Set { p } else { len - p };
                Ok(())
            }
            SeekType::Cur => {
                let new = self.pos as i64 + pos;
                if new < 0 {
                    self.pos = 0;
                    return Err(eso_error!("Error: Invalid buffer position 0x{:016X} before start of buffer!", new));
                }
                if new as usize >= len {
                    self.pos = len;
                    return Err(eso_error!("Error: Invalid buffer position 0x{:016X} past end of buffer!", new));
                }
                self.pos = new as usize;
                Ok(())
            }
        }
    }

    fn tell(&mut self) -> u64 {
        self.pos as u64
    }
}
