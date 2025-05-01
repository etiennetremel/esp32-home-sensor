use core::ffi::CStr;
use heapless::Vec;

#[derive(Debug)]
pub enum Error {
    BufferOverflow,
    InteriorNul,
}

const TLS_BUF_MAX: usize = 2048;

pub fn write_trimmed_c_str<'buf>(s: &str, buffer: &'buf mut [u8]) -> Result<&'buf CStr, Error> {
    let trimmed = s.trim_matches('\n');
    let bytes = trimmed.as_bytes();
    let len = bytes.len();

    if len + 1 > buffer.len() {
        return Err(Error::BufferOverflow);
    }

    buffer[..len].copy_from_slice(bytes);
    buffer[len] = 0;

    CStr::from_bytes_with_nul(&buffer[..=len]).map_err(|_| Error::InteriorNul)
}

pub fn build_trimmed_c_str_vec(s: &str) -> Vec<u8, TLS_BUF_MAX> {
    let trimmed = s.trim_matches('\n');
    let len = trimmed.len();
    assert!(
        len + 1 <= TLS_BUF_MAX,
        "input ({} bytes) exceeds TLS_BUF_MAX",
        len
    );

    let mut buf: Vec<u8, TLS_BUF_MAX> = Vec::new();
    buf.extend_from_slice(trimmed.as_bytes()).unwrap();
    buf.push(0).unwrap();

    buf
}
