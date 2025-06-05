use core::ffi::CStr;
use heapless::Vec;

use crate::constants::TLS_BUFFER_MAX;

#[derive(Debug)]
pub enum Error {
    BufferOverflow,
    InteriorNul,
}

// Writes a C-style string (null-terminated) to the provided buffer.
// The input string `s` is trimmed of newline characters before being written.
// Returns a `CStr` referencing the data in `buffer` or an `Error` if:
// - The buffer is too small to hold the trimmed string and the null terminator.
// - The trimmed string contains interior null bytes (which is invalid for `CStr`).
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

// Builds a `heapless::Vec<u8, TLS_BUFFER_MAX>` containing a C-style string (null-terminated).
// The input string `s` is trimmed of newline characters.
// Asserts that the length of the trimmed string plus the null terminator
// does not exceed `TLS_BUFFER_MAX`.
pub fn build_trimmed_c_str_vec(s: &str) -> Vec<u8, TLS_BUFFER_MAX> {
    let trimmed = s.trim_matches('\n');
    let len = trimmed.len();
    assert!(
        len < TLS_BUFFER_MAX,
        "input ({} bytes) exceeds TLS_BUFFER_MAX",
        len
    );

    let mut buf: Vec<u8, TLS_BUFFER_MAX> = Vec::new();
    buf.extend_from_slice(trimmed.as_bytes()).unwrap();
    buf.push(0).unwrap();

    buf
}
