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
pub(super) fn literal(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 4);
    quoted.push_str(r"\Q");
    for (i, part) in text.split(r"\E").enumerate() {
        if i != 0 {
            quoted.push_str(r"\E\\E\Q");
        }
        quoted.push_str(part);
    }
    quoted.push_str(r"\E");
    quoted
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
fn compile_flags(syntax: crate::domain::pattern::CompileOptions, dotall: bool) -> u32 {
    const MULTILINE: u32 = 0x400;
    const CASELESS: u32 = 8;
    const DOTALL: u32 = 0x20;
    const UNICODE: u32 = 0x80000 | 0x20000;
    MULTILINE | (u32::from(syntax.fold) * CASELESS) | (u32::from(dotall) * DOTALL)
        | (u32::from(!syntax.ascii) * UNICODE)
}

fn compile_pattern(pattern: &str, syntax: crate::domain::pattern::CompileOptions) -> String {
    let prefix = if syntax.crlf { "(*CRLF)" } else { "" };
    match syntax.scope {
        super::MatchScope::Anywhere => format!("{prefix}(?:{pattern})"),
        super::MatchScope::Word => format!(r"{prefix}(?<!\w)(?:{pattern})(?!\w)"),
        super::MatchScope::Line => format!("{prefix}^(?:{pattern})$"),
    }
}

impl Data<'_> {
    fn execute(&mut self, code: NonNull<c_void>, text: &str, offset: usize) -> Option<&[usize]> {
        let data = self.1.as_ptr();
        let n = unsafe {
            (self.0.exec)(code.as_ptr(), text.as_ptr(), text.len(), offset, 0, data, std::ptr::null_mut())
        };
        if n <= 0 {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts((self.0.vector)(data), n as usize * 2) })
    }
}

enum OffsetProjection {
    Emit,
    Retry,
    End,
}

impl OffsetProjection {
    fn apply(cursor: &mut crate::domain::pattern::MatchCursor, text: &str, a: usize, b: usize) -> Self {
        if !(text.is_char_boundary(a) && text.is_char_boundary(b)) {
            return Self::advance_to_boundary(cursor, text, a..b);
        }
        if cursor.consume(text, a, b) { Self::Emit } else { Self::Retry }
    }

    fn advance_to_boundary(cursor: &mut crate::domain::pattern::MatchCursor, text: &str, span: std::ops::Range<usize>) -> Self {
        // The UTF-8 renderer needs scalar boundaries in both engines.
        cursor.offset = text.ceil_char_boundary(span.end.max(cursor.offset + 1));
        if cursor.offset == text.len() && span.start == span.end { Self::End } else { Self::Retry }
    }
}

impl Pcre {
    pub fn new(
        pattern: &str,
        syntax: crate::domain::pattern::CompileOptions,
        dotall: bool,
    ) -> Result<Self, String> {
        let api = api()?;
        let mut error = 0;
        let mut offset = 0;
        let opts = compile_flags(syntax, dotall);
        let pattern = compile_pattern(pattern, syntax);
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
        self.map_matches(text, 0, |slots| slots[0]..slots[1])
            .filter(|span| span.start <= span.end)
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
        start: usize,
    ) -> impl Iterator<Item = Vec<Option<Range<usize>>>> + 'a {
        self.map_matches(text, start, move |slots| {
            slots
                .chunks_exact(2)
                .map(|s| {
                    (s[0] != usize::MAX
                        && s[0] <= s[1]
                        && text.is_char_boundary(s[0])
                        && text.is_char_boundary(s[1]))
                    .then_some(s[0]..s[1])
                })
                .collect()
        })
    }

    fn map_matches<'a, T>(
        &'a self,
        text: &'a str,
        start: usize,
        mut project: impl FnMut(&[usize]) -> T + 'a,
    ) -> impl Iterator<Item = T> + 'a {
        let mut cursor = crate::domain::pattern::MatchCursor::new(start);
        let mut data =
            NonNull::new(unsafe { (self.api.data)(self.code.as_ptr(), std::ptr::null_mut()) })
                .map(|p| Data(self.api, p));
        std::iter::from_fn(move || {
            let data = data.as_mut()?;
            while cursor.offset <= text.len() {
                let slots = data.execute(self.code, text, cursor.offset)?;
                match OffsetProjection::apply(&mut cursor, text, slots[0], slots[1]) {
                    OffsetProjection::Emit => return Some(project(slots)),
                    OffsetProjection::Retry => {}
                    OffsetProjection::End => return None,
                }
            }
            None
        })
    }
}
