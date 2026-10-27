//! Port of EsoCommon.cpp: byte-swapping, buffer parsing, zlib/snappy helpers, path and string helpers,
//! and the magic-byte file type guesser.

use crate::error::{EsoError, Result};
use crate::{eso_error, print_error};
use std::fs;
use std::path::{Path, PathBuf, MAIN_SEPARATOR};

pub type Byte = u8;
pub type Word = u16;
pub type Dword = u32;
pub type Dword64 = u64;

// ---------------------------------------------------------------------------------------------
// Byte swapping / buffer parsing
// ---------------------------------------------------------------------------------------------

pub fn word_swap(v: u16) -> u16 { v.swap_bytes() }
pub fn dword_swap(v: u32) -> u32 { v.swap_bytes() }
pub fn dword64_swap(v: u64) -> u64 { v.swap_bytes() }
pub fn float_swap(v: f32) -> f32 { f32::from_bits(v.to_bits().swap_bytes()) } // C++ version was an assert(false) TODO

fn array_at<const N: usize>(buf: &[u8], offset: usize) -> Option<[u8; N]> {
    buf.get(offset..offset.checked_add(N)?)?.try_into().ok()
}

/// `is_big_endian == true` means the data in the buffer is big endian (matches the C++ flag).
pub fn parse_buffer_word(buf: &[u8], is_big_endian: bool) -> u16 {
    let b = array_at::<2>(buf, 0).unwrap_or([0; 2]);
    if is_big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
}

pub fn parse_buffer_dword(buf: &[u8], is_big_endian: bool) -> u32 {
    let b = array_at::<4>(buf, 0).unwrap_or([0; 4]);
    if is_big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
}

/// 3-byte integer.
pub fn parse_buffer_dword3(buf: &[u8], is_big_endian: bool) -> u32 {
    let b = array_at::<3>(buf, 0).unwrap_or([0; 3]);
    if is_big_endian {
        ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32
    } else {
        b[0] as u32 | ((b[1] as u32) << 8) | ((b[2] as u32) << 16)
    }
}

/// Reads a NUL-terminated string starting at `offset`, never reading past `size`.
pub fn parse_buffer_string(buf: &[u8], offset: usize, size: usize) -> String {
    let end = size.min(buf.len());
    if offset >= end {
        return String::new();
    }
    let slice = &buf[offset..end];
    let len = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(&slice[..len]).into_owned()
}

/// C `atoi` (clamps to the i32 range like the MSVC CRT).
pub fn atoi(s: &str) -> i32 {
    let s = s.trim_start();
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut v: i64 = 0;
    for b in rest.bytes() {
        if !b.is_ascii_digit() { break; }
        v = (v * 10 + (b - b'0') as i64).min(i64::from(i32::MAX) + 1);
    }
    if neg { v = -v; }
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// C `strtoul(s, NULL, 10)` with a 32-bit `unsigned long` (Windows).
pub fn strtoul(s: &str) -> u32 {
    let s = s.trim_start();
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut v: u64 = 0;
    for b in rest.bytes() {
        if !b.is_ascii_digit() { break; }
        v = (v * 10 + (b - b'0') as u64).min(u64::from(u32::MAX) + 1);
    }
    let v = v.min(u64::from(u32::MAX)) as u32;
    if neg { v.wrapping_neg() } else { v }
}

/// Equivalent of `sscanf(s, "%u-%u-%u", ...)`.
pub fn parse_id_triplet(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.splitn(3, '-');
    let a = it.next()?;
    let b = it.next()?;
    let c = it.next()?;
    let ok = |x: &str| x.trim_start().bytes().next().map_or(false, |c| c.is_ascii_digit());
    if !ok(a) || !ok(b) || !ok(c) { return None; }
    Some((strtoul(a), strtoul(b), strtoul(c)))
}

// ---------------------------------------------------------------------------------------------
// Compression helpers
// ---------------------------------------------------------------------------------------------

fn make_err(quiet: bool, msg: &str) -> EsoError {
    if quiet { EsoError::new(msg) } else { eso_error!("{}", msg) }
}

/// Inflates a complete zlib stream into `output`; returns the number of bytes produced.
///
/// Like the C++ version a truncated stream (input exhausted before the stream end marker) is
/// accepted.  With `quiet` nothing is logged.
///
/// NOTE: this uses flate2's default pure-Rust backend.  Code that relies on *exactly* how many input
/// bytes zlib consumed before hitting a data error (the ZOSFT parser) may behave differently from the
/// C zlib; enable flate2's `zlib` or `zlib-rs` feature if that turns out to matter.
pub fn inflate_zlib_block(output: &mut [u8], input: &[u8], quiet: bool) -> Result<usize> {
    use flate2::{Decompress, FlushDecompress, Status};
    let mut d = Decompress::new(true);
    loop {
        let in_pos = d.total_in() as usize;
        let out_pos = d.total_out() as usize;
        match d.decompress(&input[in_pos..], &mut output[out_pos..], FlushDecompress::None) {
            Ok(Status::StreamEnd) => break,
            Ok(Status::BufError) => {
                if in_pos >= input.len() { break; } // truncated input treated as end of stream
                return Err(make_err(quiet, "Error: Output buffer too small to uncompress data stream!"));
            }
            Ok(Status::Ok) => {
                if d.total_in() as usize == in_pos && d.total_out() as usize == out_pos {
                    return Err(make_err(quiet, "Error: Failed to uncompress data stream!"));
                }
            }
            Err(_) => return Err(make_err(quiet, "Error: Failed to uncompress data stream!")),
        }
    }
    Ok(d.total_out() as usize)
}

/// Deflates `input` (zlib header, default level) into `output`; returns the compressed size.
pub fn deflate_zlib_block(output: &mut [u8], input: &[u8], quiet: bool) -> Result<usize> {
    use flate2::{Compress, Compression, FlushCompress, Status};
    let mut c = Compress::new(Compression::default(), true);
    loop {
        let in_pos = c.total_in() as usize;
        let out_pos = c.total_out() as usize;
        match c.compress(&input[in_pos..], &mut output[out_pos..], FlushCompress::Finish) {
            Ok(Status::StreamEnd) => break,
            Ok(Status::BufError) => {
                if out_pos >= output.len() { return Err(make_err(quiet, "Error: No more output space available!")); }
                break;
            }
            Ok(Status::Ok) => {
                if c.total_in() as usize == in_pos && c.total_out() as usize == out_pos {
                    return Err(make_err(quiet, "Error: No more output space available!"));
                }
            }
            Err(_) => return Err(make_err(quiet, "Error: Failed to compress data stream!")),
        }
    }
    Ok(c.total_out() as usize)
}

/// Decompresses a raw (unframed) snappy block into `output`; returns the decompressed size.
pub fn inflate_snappy_block(output: &mut [u8], input: &[u8], quiet: bool) -> Result<usize> {
    if input.len() > i32::MAX as usize {
        return Err(make_err(quiet, &format!("Error: Compressed snappy data size is too large ({} byte)!", input.len())));
    }
    let real = snap::raw::decompress_len(input)
        .map_err(|_| make_err(quiet, "Error: Invalid Snappy data format found!"))?;
    if real > i32::MAX as usize {
        return Err(make_err(quiet, &format!("Error: Uncompressed snappy data size is too large ({} bytes)!", real)));
    }
    if real > output.len() {
        return Err(make_err(quiet, &format!("Error: Uncompressed snappy data size exceeds in the output buffer ({} bytes)!", real)));
    }
    snap::raw::Decoder::new()
        .decompress(input, &mut output[..real])
        .map_err(|_| make_err(quiet, "Error: Failed to decompress the Snappy data!"))
}

/// Equivalent of `snappy::IsValidCompressedBuffer`.
pub fn is_valid_snappy(input: &[u8]) -> bool {
    snap::raw::Decoder::new().decompress_vec(input).is_ok()
}

// ---------------------------------------------------------------------------------------------
// Strings / paths
// ---------------------------------------------------------------------------------------------

pub fn string_ends_with(full: &str, ending: &str) -> bool {
    full.ends_with(ending)
}

pub fn string_ends_with_ignore_case(full: &str, ending: &str) -> bool {
    full.to_lowercase().ends_with(&ending.to_lowercase())
}

/// C++ `ReplaceStrings` (replace every non-overlapping occurrence).
pub fn replace_strings(subject: &str, search: &str, replace: &str) -> String {
    if search.is_empty() { return subject.to_string(); }
    subject.replace(search, replace)
}

/// Splits like `std::getline` in a loop (no trailing empty element).
pub fn split_string(s: &str, delim: char) -> Vec<String> {
    let mut v: Vec<String> = s.split(delim).map(String::from).collect();
    if v.last().map_or(false, |l| l.is_empty()) { v.pop(); }
    v
}

fn normalize_separators(s: &str) -> String {
    s.chars().map(|c| if c == '/' || c == '\\' { MAIN_SEPARATOR } else { c }).collect()
}

pub fn remove_file_extension(filename: &str) -> String {
    let mut out = normalize_separators(filename);
    let bytes = out.as_bytes();
    let mut cut = None;
    for i in (1..=bytes.len()).rev() {
        let c = bytes[i - 1];
        if c == MAIN_SEPARATOR as u8 || c == b':' { break; }
        if c == b'.' { cut = Some(i - 1); break; }
    }
    if let Some(i) = cut { out.truncate(i); }
    out
}

pub fn remove_filename(filename: &str) -> String {
    let mut out = normalize_separators(filename);
    let bytes = out.as_bytes();
    for i in (1..=bytes.len()).rev() {
        let c = bytes[i - 1];
        if c == MAIN_SEPARATOR as u8 || c == b':' { out.truncate(i); break; }
    }
    out
}

/// Directory part of a path including the trailing separator (`ExtractPath`; not in the supplied sources).
pub fn extract_path(filename: &str) -> String {
    remove_filename(filename)
}

/// True if a relative path tries to climb out of its base directory (`..` component) or is absolute.
/// (Added in the port: ZOSFT/GR2 names come from game data and were joined onto the output path unchecked.)
pub fn has_unsafe_component(path: &str) -> bool {
    let p = normalize_separators(path);
    p.split(MAIN_SEPARATOR).any(|c| c == "..") || p.starts_with(MAIN_SEPARATOR) || p.contains(':')
}

pub fn terminate_path(path: &str) -> String {
    let mut out = normalize_separators(path);
    if !out.is_empty() && !out.ends_with(MAIN_SEPARATOR) { out.push(MAIN_SEPARATOR); }
    out
}

pub fn append_paths(path1: &str, path2: &str) -> String {
    let mut out = terminate_path(path1);
    out.push_str(path2.strip_prefix(['\\', '/']).unwrap_or(path2));
    terminate_path(&out)
}

pub fn append_filename_to_path(path: &str, filename: &str) -> String {
    let mut out = terminate_path(path);
    out.push_str(filename.trim_start_matches(['\\', '/']));
    out
}

/// `CreateFilename(base, fmt, ...)`: callers use `format!` for the suffix.
pub fn create_filename(base: &str, suffix: &str) -> String {
    let mut out = normalize_separators(base);
    let skip = out.ends_with(MAIN_SEPARATOR) && suffix.starts_with('\\');
    out.push_str(if skip { &suffix[1..] } else { suffix });
    out
}

pub fn ensure_path_exists(path: &str) -> bool {
    if directory_exists(path) { return true; }
    match fs::create_dir_all(path) {
        Ok(()) => true,
        Err(_) => { print_error!("Error: Failed to create the directory '{}'!", path); false }
    }
}

pub fn directory_exists(path: &str) -> bool { Path::new(path).is_dir() }
pub fn file_exists(path: &str) -> bool { Path::new(path).is_file() }

pub fn get_file_size(filename: &str) -> Option<u64> {
    fs::metadata(filename).ok().map(|m| m.len())
}

/// Minimal `*` / `?` wildcard match (case-insensitive like Windows).
fn wildcard_match(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|i| wildcard_match(rest, &name[i..])),
        Some((b'?', rest)) => !name.is_empty() && wildcard_match(rest, &name[1..]),
        Some((c, rest)) => name.first().map_or(false, |n| n.eq_ignore_ascii_case(c)) && wildcard_match(rest, &name[1..]),
    }
}

/// Equivalent of `FindFirstFile/FindNextFile` on a spec like `C:\data\*.dat` (regular files only).
pub fn find_files(file_spec: &str) -> std::io::Result<Vec<PathBuf>> {
    let spec = normalize_separators(file_spec);
    let p = Path::new(&spec);
    let dir = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let pattern = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "*".into());
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_file() && wildcard_match(pattern.as_bytes(), name.as_bytes()) {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

pub fn get_files_size(file_spec: &str) -> Option<u64> {
    let files = find_files(file_spec).ok()?;
    Some(files.iter().filter_map(|f| fs::metadata(f).ok()).map(|m| m.len()).sum())
}

pub fn get_folder_size(path: &str) -> Option<u64> {
    fn walk(p: &Path) -> std::io::Result<u64> {
        let mut total = 0;
        for e in fs::read_dir(p)? {
            let e = e?;
            let md = e.metadata()?;
            total += if md.is_dir() { walk(&e.path())? } else { md.len() };
        }
        Ok(total)
    }
    walk(Path::new(path)).ok()
}

pub fn delete_files(file_spec: &str) -> bool {
    match find_files(file_spec) {
        Ok(files) => files.iter().fold(true, |ok, f| fs::remove_file(f).is_ok() && ok),
        Err(_) => false,
    }
}

pub fn get_timer_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// The C++ version used DevIL (only when `__UESP_NODEVIL` is undefined).  Not implemented.
pub fn convert_dds_to_png(_data: &[u8], _filename: &str) -> Result<()> {
    Err(eso_error!("Not currently implemented!"))
}

// ---------------------------------------------------------------------------------------------
// File type guessing
// ---------------------------------------------------------------------------------------------

fn at(buf: &[u8], off: usize, pat: &[u8]) -> bool {
    off.checked_add(pat.len()).map_or(false, |e| e <= buf.len()) && &buf[off..off + pat.len()] == pat
}

fn from_end(buf: &[u8], back: usize, pat: &[u8]) -> bool {
    buf.len() >= back && at(buf, buf.len() - back, pat)
}

/// Guesses a file extension from the leading (and sometimes trailing) bytes.  Order matters.
pub fn guess_file_extension(buf: &[u8]) -> &'static str {
    const GR2_MAGIC: [&[u8; 4]; 8] = [
        b"\x29\xDE\x6C\xC0", b"\xE5\x9B\x49\x5E", b"\x29\x75\x31\x82", b"\x0E\x11\x95\xB5",
        b"\x0E\x74\xA2\x0A", b"\xE5\x2F\x4A\xE1", b"\x31\x95\xD4\xE3", b"\x31\xC2\x4E\x7C",
    ];
    let s = |p: &[u8]| at(buf, 0, p);

    if s(b"DDS ") { "dds" }
    else if s(b"\x00\x01\x00\x00\x00\x0E\x00\x80") || s(b"OTTO") || at(buf, 11, b"POS/2") { "ttf" }
    else if s(b"\x1E\x0D\x0B\xCD\xCE\xFA\x11") { "hk" }
    else if GR2_MAGIC.iter().any(|m| s(*m)) { "gr2" }
    else if s(b"\x1E\x0D\xB0\xCA") { "hkx" }
    else if s(b"\xFA\xFA\xEB\xEB") { "EsoFileData" }
    else if s(b"\xFB\xFB\xEC\xEC") || s(b"\x00\x00\x00\x02") { "EsoIdData" }
    else if s(b"\xEF\xBB\xBF") { "txt" }
    else if s(b"xV4") { "xv4" }
    else if s(b"__ffx") { "ffx" }
    else if s(b"RIFF") { "riff" }
    else if s(b"; ") || from_end(buf, 4, b".lua") { "txt" }
    else if s(b"#") || s(b"//") || s(b"\r\n#") || s(b"/*") { "fx" }
    else if s(b"--") || s(b"local") || s(b"function") { "lua" }
    else if s(b"<") { "xml" }
    else if s(b"ZOSFT") { "zosft" }
    else if s(b"PSB2") { "psb2" }
    else if from_end(buf, 5, b"end") || from_end(buf, 3, b"end") || from_end(buf, 7, b"end")
        || from_end(buf, 2, b"\r\0") { "lua" }
    else { "dat" }
}