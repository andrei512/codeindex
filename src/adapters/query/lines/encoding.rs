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
            "auto" => Ok(Self::Auto),
            "none" => Ok(Self::None),
            "utf-8" | "utf8" => Ok(Self::Utf8),
            legacy => Self::legacy(value, legacy),
        }
    }

    fn legacy(value: &str, label: &str) -> Result<Self, String> {
        let label = match label {
            "sjis" | "shift-jis" | "shift_jis" => "CP932",
            "iso-8859-1" | "latin1" | "ascii" => "WINDOWS-1252",
            "replacement" | "csiso2022kr" | "utf-7" => {
                return Err(format!("unknown encoding: {value}"));
            }
            s => s,
        };
        let name = CString::new(label).map_err(|_| "invalid encoding")?;
        let _probe = Converter::open(&name).ok_or_else(|| format!("unknown encoding: {value}"))?;
        Ok(Self::Legacy(name))
    }

    pub fn decode<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if matches!(self, Self::None) {
            return bytes.into();
        }
        if let Some(body) = bytes.strip_prefix(b"\xef\xbb\xbf") {
            return body.into();
        }
        if let Some(body) = bytes.strip_prefix(b"\xff\xfe") {
            return decode_utf16(body, u16::from_le_bytes).into();
        }
        if let Some(body) = bytes.strip_prefix(b"\xfe\xff") {
            return decode_utf16(body, u16::from_be_bytes).into();
        }
        let Self::Legacy(name) = self else {
            return bytes.into();
        };
        decode_legacy(name, bytes).map_or(Cow::Borrowed(bytes), Cow::Owned)
    }
}

fn decode_utf16(bytes: &[u8], word: impl Fn([u8; 2]) -> u16) -> Vec<u8> {
    let (words, remainder) = bytes.as_chunks::<2>();
    let mut text: String = char::decode_utf16(words.iter().copied().map(word))
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    if !remainder.is_empty() {
        text.push(char::REPLACEMENT_CHARACTER);
    }
    text.into_bytes()
}

fn decode_legacy(name: &CString, bytes: &[u8]) -> Option<Vec<u8>> {
    let conversion = Converter::open(name)?;
    let cd = conversion.0;
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
    Some(result)
}

struct Converter(*mut c_void);

impl Converter {
    fn open(name: &CString) -> Option<Self> {
        let handle = unsafe { iconv_open(c"UTF-8".as_ptr(), name.as_ptr()) };
        (handle as isize != -1).then(|| Self(handle))
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        unsafe { iconv_close(self.0) };
    }
}
