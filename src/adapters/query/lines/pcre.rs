//! Optional PCRE2-8 engine selected by -P or --engine=auto. Loading the OS
//! library on demand keeps ordinary searches independent of its installation.
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    ops::Range,
    ptr::NonNull,
    sync::OnceLock,
};
type Ptr = *mut c_void;
#[cfg_attr(target_os = "linux", link(name = "dl"))]
unsafe extern "C" {
    fn dlopen(name: *const c_char, mode: c_int) -> Ptr;
    fn dlsym(handle: Ptr, name: *const c_char) -> Ptr;
}
struct Api {
    compile: unsafe extern "C" fn(*const u8, usize, u32, *mut i32, *mut usize, Ptr) -> Ptr,
    free: unsafe extern "C" fn(Ptr),
    data: unsafe extern "C" fn(Ptr, Ptr) -> Ptr,
    free_data: unsafe extern "C" fn(Ptr),
    exec: unsafe extern "C" fn(Ptr, *const u8, usize, usize, u32, Ptr, Ptr) -> i32,
    vector: unsafe extern "C" fn(Ptr) -> *const usize,
    number: unsafe extern "C" fn(Ptr, *const u8) -> i32,
    error: unsafe extern "C" fn(i32, *mut u8, usize) -> i32,
    config: unsafe extern "C" fn(u32, Ptr) -> i32,
}
fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let names = &[
            c"/opt/homebrew/lib/libpcre2-8.0.dylib",
            c"/usr/local/lib/libpcre2-8.0.dylib",
            c"/opt/local/lib/libpcre2-8.0.dylib",
        ][..];
        #[cfg(not(target_os = "macos"))]
        let names = &[c"libpcre2-8.so.0"][..];
        let handle = names
            .iter()
            .find_map(|name| NonNull::new(unsafe { dlopen(name.as_ptr(), 2) }))?;
        // PCRE2 exposes a versioned C ABI with suffixed symbol names. Keep
        // this handle for the process lifetime so the cached pointers stay valid.
        macro_rules! symbol {
            ($name:literal) => {{
                let ptr = NonNull::new(unsafe {
                    dlsym(handle.as_ptr(), concat!($name, "_8\0").as_ptr().cast())
                })?;
                unsafe { std::mem::transmute(ptr.as_ptr()) }
            }};
        }
        Some(Api {
            compile: symbol!("pcre2_compile"),
            free: symbol!("pcre2_code_free"),
            data: symbol!("pcre2_match_data_create_from_pattern"),
            free_data: symbol!("pcre2_match_data_free"),
            exec: symbol!("pcre2_match"),
            vector: symbol!("pcre2_get_ovector_pointer"),
            number: symbol!("pcre2_substring_number_from_name"),
            error: symbol!("pcre2_get_error_message"),
            config: symbol!("pcre2_config"),
        })
    })
    .as_ref()
    .ok_or_else(|| {
        "PCRE2 is not available. Install the system PCRE2-8 library to enable -P.".into()
    })
}
pub(crate) fn version() -> Result<String, String> {
    let api = api()?;
    let mut version = [0u8; 128];
    let mut jit = 0u32;
    unsafe {
        (api.config)(11, version.as_mut_ptr().cast());
        (api.config)(1, (&mut jit as *mut u32).cast());
    }
    let version = CStr::from_bytes_until_nul(&version)
        .map_err(|e| e.to_string())?
        .to_string_lossy();
    Ok(format!(
        "PCRE2 {} is available (JIT is {}available)",
        version.split_whitespace().next().unwrap_or(""),
        if jit == 0 { "not " } else { "" }
    ))
}
pub(super) struct Pcre {
    api: &'static Api,
    code: NonNull<c_void>,
}
// Compiled code is immutable. Every executing iterator owns separate match data.
unsafe impl Sync for Pcre {}
unsafe impl Send for Pcre {}
impl Drop for Pcre {
    fn drop(&mut self) {
        unsafe { (self.api.free)(self.code.as_ptr()) }
    }
}
struct Data<'a>(&'a Api, NonNull<c_void>);
impl Drop for Data<'_> {
    fn drop(&mut self) {
        unsafe { (self.0.free_data)(self.1.as_ptr()) }
    }
}
impl Pcre {
    pub fn new(
        pattern: &str,
        fold: bool,
        dotall: bool,
        unicode: bool,
        crlf: bool,
    ) -> Result<Self, String> {
        let api = api()?;
        let mut error = 0;
        let mut offset = 0;
        let opts = 0x400
            | if fold { 8 } else { 0 }
            | if dotall { 0x20 } else { 0 }
            | if unicode { 0x80000 | 0x20000 } else { 0 };
        let pattern = if crlf {
            format!("(*CRLF)(?:{pattern})")
        } else {
            format!("(?:{pattern})")
        };
        let p = unsafe {
            (api.compile)(
                pattern.as_ptr(),
                pattern.len(),
                opts,
                &mut error,
                &mut offset,
                std::ptr::null_mut(),
            )
        };
        let code = NonNull::new(p).ok_or_else(|| {
            let mut msg = [0u8; 256];
            let n = unsafe { (api.error)(error, msg.as_mut_ptr(), msg.len()) }.max(0) as usize;
            format!(
                "PCRE2: error compiling pattern at offset {offset}: {}",
                String::from_utf8_lossy(&msg[..n])
            )
        })?;
        Ok(Self { api, code })
    }
    pub fn matches<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        self.captures(text, 0)
            .filter_map(|groups| groups.into_iter().next().flatten())
    }
    pub fn id(&self, name: &str) -> Option<usize> {
        name.parse().ok().or_else(|| {
            let c = CString::new(name).ok()?;
            usize::try_from(unsafe { (self.api.number)(self.code.as_ptr(), c.as_ptr().cast()) })
                .ok()
        })
    }
    pub fn captures<'a>(
        &'a self,
        text: &'a str,
        mut cursor: usize,
    ) -> impl Iterator<Item = Vec<Option<Range<usize>>>> + 'a {
        let data =
            NonNull::new(unsafe { (self.api.data)(self.code.as_ptr(), std::ptr::null_mut()) })
                .map(|p| Data(self.api, p));
        let mut previous = None;
        std::iter::from_fn(move || {
            loop {
                if cursor > text.len() {
                    return None;
                }
                let d = data.as_ref()?.1.as_ptr();
                let n = unsafe {
                    (self.api.exec)(
                        self.code.as_ptr(),
                        text.as_ptr(),
                        text.len(),
                        cursor,
                        0,
                        d,
                        std::ptr::null_mut(),
                    )
                };
                if n <= 0 {
                    return None;
                }
                let slots =
                    unsafe { std::slice::from_raw_parts((self.api.vector)(d), n as usize * 2) };
                let (a, b) = (slots[0], slots[1]);
                // cx's UTF-8 renderer needs scalar boundaries in both engines.
                if !text.is_char_boundary(a) || !text.is_char_boundary(b) {
                    cursor = text.ceil_char_boundary(b.max(cursor + 1));
                    if cursor == text.len() && a == b {
                        return None;
                    }
                    continue;
                }
                let duplicate = a == b && previous == Some(b);
                previous = Some(b);
                cursor = if a == b {
                    b + text[b..].chars().next().map_or(1, char::len_utf8)
                } else {
                    b
                };
                if !duplicate {
                    return Some(
                        slots
                            .chunks_exact(2)
                            .map(|s| {
                                (s[0] != usize::MAX
                                    && s[0] <= s[1]
                                    && text.is_char_boundary(s[0])
                                    && text.is_char_boundary(s[1]))
                                .then_some(s[0]..s[1])
                            })
                            .collect(),
                    );
                }
            }
        })
    }
}
