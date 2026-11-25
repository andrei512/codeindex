//! Decode search input locally. The ordinary UTF-8 path borrows its bytes.
use std::{
    borrow::Cow,
    ffi::{CString, c_char, c_void},
    ptr,
};

#[cfg_attr(target_os = "macos", link(name = "iconv"))]
unsafe extern "C" {
    fn iconv_open(to: *const c_char, from: *const c_char) -> *mut c_void;
    fn iconv(
        cd: *mut c_void,
        input: *mut *mut c_char,
        left: *mut usize,
        output: *mut *mut c_char,
        capacity: *mut usize,
    ) -> usize;
    fn iconv_close(cd: *mut c_void) -> i32;
}
#[derive(Default)]
pub(super) enum Encoding {
    #[default]
    Auto,
    None,
    Utf8,
    Legacy(CString),
}
impl Encoding {
    pub fn parse(value: &str) -> Result<Self, String> {
        let label = value.to_ascii_lowercase();
        match label.as_str() {
            "auto" => return Ok(Self::Auto),
            "none" => return Ok(Self::None),
            "utf-8" | "utf8" => return Ok(Self::Utf8),
            _ => {}
        }
        let label = match label.as_str() {
            "sjis" | "shift-jis" | "shift_jis" => "CP932",
            "iso-8859-1" | "latin1" | "ascii" => "WINDOWS-1252",
            "replacement" | "csiso2022kr" | "utf-7" => {
                return Err(format!("unknown encoding: {value}"));
            }
            s => s,
        };
        let name = CString::new(label).map_err(|_| "invalid encoding")?;
        let cd = unsafe { iconv_open(c"UTF-8".as_ptr(), name.as_ptr()) };
        if cd as isize == -1 {
            return Err(format!("unknown encoding: {value}"));
        }
        unsafe { iconv_close(cd) };
        Ok(Self::Legacy(name))
    }

    pub fn decode<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if matches!(self, Self::None) {
            return bytes.into();
        }
        if let Some(body) = bytes.strip_prefix(b"\xef\xbb\xbf") {
            return body.into();
        }
        for (bom, little) in [(&b"\xff\xfe"[..], true), (&b"\xfe\xff"[..], false)] {
            if let Some(body) = bytes.strip_prefix(bom) {
                let words = body.chunks_exact(2).map(|pair| {
                    if little {
                        u16::from_le_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_be_bytes([pair[0], pair[1]])
                    }
                });
                let mut s: String = char::decode_utf16(words)
                    .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect();
                if body.len() % 2 != 0 {
                    s.push(char::REPLACEMENT_CHARACTER);
                }
                return s.into_bytes().into();
            }
        }
        let Self::Legacy(name) = self else {
            return bytes.into();
        };
        let cd = unsafe { iconv_open(c"UTF-8".as_ptr(), name.as_ptr()) };
        if cd as isize == -1 {
            return bytes.into();
        }
        let mut left = bytes.len();
        let mut input = bytes.as_ptr() as *mut c_char;
        let mut result = Vec::new();
        let mut buffer = [0u8; 8192];
        while left > 0 {
            let mut dest = buffer.as_mut_ptr().cast();
            let mut capacity = buffer.len();
            let status = unsafe { iconv(cd, &mut input, &mut left, &mut dest, &mut capacity) };
            result.extend_from_slice(&buffer[..buffer.len() - capacity]);
            if status == usize::MAX && capacity >= 4 && left > 0 {
                result.extend_from_slice("�".as_bytes());
                input = unsafe { input.add(1) };
                left -= 1;
                unsafe {
                    iconv(
                        cd,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                };
            }
        }
        unsafe { iconv_close(cd) };
        result.into()
    }
}
