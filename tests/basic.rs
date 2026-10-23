use eso_common::csv::CsvFile;
use eso_common::lang::EsoLangFile;
use eso_common::util::*;

#[test]
fn csv_quotes_newlines_and_empty_cells() {
    let mut c = CsvFile::new(true);
    c.parse(b"\"a\",\"b, \"\"q\"\"\",\r\n\"1\",,\"x\ny\"\n");
    assert_eq!(c.num_rows(), 2);
    assert_eq!(c.data()[0], vec!["a", "b, \"\"q\"\"", ""]);
    assert_eq!(c.data()[1], vec!["1", "", "x\ny"]);
    assert_eq!(c.num_data_rows(), 1);
}

#[test]
fn zlib_roundtrip() {
    let src: Vec<u8> = (0..5000u32).map(|i| (i % 7) as u8).collect();
    let mut comp = vec![0u8; 6000];
    let n = deflate_zlib_block(&mut comp, &src, false).unwrap();
    let mut out = vec![0u8; src.len()];
    let m = inflate_zlib_block(&mut out, &comp[..n], false).unwrap();
    assert_eq!(&out[..m], &src[..]);
}

#[test]
fn snappy_roundtrip() {
    let src = b"hello hello hello hello snappy".to_vec();
    let comp = snap::raw::Encoder::new().compress_vec(&src).unwrap();
    assert!(is_valid_snappy(&comp));
    let mut out = vec![0u8; 100];
    let n = inflate_snappy_block(&mut out, &comp, true).unwrap();
    assert_eq!(&out[..n], &src[..]);
}

#[test]
fn guess_extensions() {
    assert_eq!(guess_file_extension(b"DDS \0\0\0\0"), "dds");
    assert_eq!(guess_file_extension(b"RIFF1234"), "riff");
    assert_eq!(guess_file_extension(b"\x29\xDE\x6C\xC0rest"), "gr2");
    assert_eq!(guess_file_extension(b"ZOSFT\0\0\0"), "zosft");
    assert_eq!(guess_file_extension(b"\x00\x00\x00\x02xxxx"), "EsoIdData");
    assert_eq!(guess_file_extension(b"x"), "dat"); // short buffers must not panic
    assert_eq!(guess_file_extension(b""), "dat");
}

#[test]
fn lang_binary_roundtrip_is_big_endian() {
    let mut l = EsoLangFile::new();
    l.add_entry(5, 0, 1, 0, "one");
    l.add_entry(6, 0, 2, 0, "two");
    let path = std::env::temp_dir().join("eso_common_test.lang");
    let p = path.to_str().unwrap();
    l.save(p).unwrap();
    let bytes = std::fs::read(p).unwrap();
    assert_eq!(&bytes[..8], &[0, 0, 0, 2, 0, 0, 0, 2]);
    let mut l2 = EsoLangFile::new();
    l2.load(p).unwrap();
    assert_eq!(l2.records()[1].text, "two");
}

#[test]
fn path_helpers() {
    assert_eq!(remove_file_extension("a/b/c.txt"), format!("a{0}b{0}c", std::path::MAIN_SEPARATOR));
    assert_eq!(split_string("a,,b,", ','), vec!["a", "", "b"]);
    assert_eq!(atoi("  -12abc"), -12);
    assert_eq!(strtoul("4000000000-1"), 4_000_000_000);
}
