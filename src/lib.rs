pub mod block;
pub mod csv;
pub mod dat;
pub mod error;
pub mod file;
pub mod granny;
pub mod lang;
pub mod log;
pub mod mnf;
pub mod oodle;
pub mod riff;
pub mod subfile;
pub mod util;
pub mod zosft;

use std::io::{self, Read, Seek, SeekFrom};
use serde::ser::SerializeSeq;
use serde::Serializer;

pub fn serialize_array<S, T, const N: usize>(arr: &[T; N], s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: serde::Serialize,
{
    let mut seq = s.serialize_seq(Some(N))?;
    for b in arr.iter() {
        seq.serialize_element(b)?;
    }
    seq.end()
}

pub fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}
pub fn is_two_u8(v: &u8) -> bool {
    *v == 2
}
pub fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}
pub fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
pub fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}
pub fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}
pub fn is_empty_str(v: &String) -> bool {
    v.is_empty()
}
pub fn is_empty_vec<T>(v: &Vec<T>) -> bool
where
    T: Default + PartialEq,
{
    v.is_empty() || v.iter().all(|f| f == &T::default())
}
pub fn is_empty_array<T, const N: usize>(v: &[T; N]) -> bool
where
    T: Default + PartialEq,
{
    v.iter().all(|f| f == &T::default())
}

pub fn always_skip_serialization<T>(_v: &T) -> bool {
    true
}

pub fn report_error(msg: &str) -> io::Error {
    eprintln!("{msg}");
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

pub struct ByteReader<R: Read + Seek> {
    inner: R,
}

impl<R: Read + Seek> ByteReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    pub fn tell(&mut self) -> io::Result<u64> {
        self.inner.stream_position()
    }

    pub fn seek(&mut self, pos: u64) -> io::Result<()> {
        self.inner.seek(SeekFrom::Start(pos))?;
        Ok(())
    }

    pub fn read_byte(&mut self) -> io::Result<u8> {
        let mut buf = [0u8; 1];
        self.inner.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    pub fn read_word_be(&mut self) -> io::Result<u16> {
        let mut buf = [0u8; 2];
        self.inner.read_exact(&mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }

    pub fn read_word_le(&mut self) -> io::Result<u16> {
        let mut buf = [0u8; 2];
        self.inner.read_exact(&mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    pub fn read_dword_be(&mut self) -> io::Result<u32> {
        let mut buf = [0u8; 4];
        self.inner.read_exact(&mut buf)?;
        Ok(u32::from_be_bytes(buf))
    }

    pub fn read_dword_le(&mut self) -> io::Result<u32> {
        let mut buf = [0u8; 4];
        self.inner.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    pub fn read_idword_be(&mut self) -> io::Result<i32> {
        let mut buf = [0u8; 4];
        self.inner.read_exact(&mut buf)?;
        Ok(i32::from_be_bytes(buf))
    }

    pub fn read_float_le(&mut self) -> io::Result<f32> {
        let bits = self.read_dword_le()?;
        Ok(f32::from_bits(bits))
    }

    pub fn read_bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.inner.read_exact(&mut buf)?;
        Ok(buf)
    }
}

impl ByteReader<std::fs::File> {
    pub fn open(filename: &str) -> io::Result<Self> {
        let f = std::fs::File::open(filename)?;
        Ok(Self::new(f))
    }

    pub fn file_size(&mut self) -> io::Result<u64> {
        let cur = self.tell()?;
        let end = self.inner.seek(SeekFrom::End(0))?;
        self.seek(cur)?;
        Ok(end)
    }
}
