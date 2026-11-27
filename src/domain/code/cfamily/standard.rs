//! A small native ABI model for standard integer headers. The rest of the
//! standard macro namespace is left unknown when its defining text is absent.
use super::condition::Integer;
use std::os::raw::{c_char, c_int, c_long, c_longlong, c_short};

#[derive(Clone, Copy)]
enum Representation {
    Signed,
    PromotedUnsigned,
    PlainChar,
    Unsigned,
}

impl Representation {
    fn minimum(self, mask: u64, unsigned_char: bool) -> Option<Integer> {
        let signed_min = !((mask >> 1) as i64);
        match self {
            PlainChar if unsigned_char => Some(Integer::signed(0)),
            PlainChar | Signed => Some(Integer::signed(signed_min)),
            PromotedUnsigned | Unsigned => None,
        }
    }

    fn maximum(self, mask: u64, bits: u32, unsigned_char: bool) -> Integer {
        match self {
            Signed => Integer::signed((mask >> 1) as i64),
            PlainChar if !unsigned_char => Integer::signed((mask >> 1) as i64),
            PlainChar | PromotedUnsigned if bits < c_int::BITS => Integer::signed(mask as i64),
            PlainChar | PromotedUnsigned | Unsigned => Integer::unsigned(mask),
        }
    }
}

use Representation::{PlainChar, PromotedUnsigned, Signed, Unsigned};

// Unsigned types narrower than int undergo integer promotion. SIZE_MAX's
// unsigned value is provided directly by the native size type.
const LIMITS: &[(&str, u32, Representation)] = &[
    ("CHAR", 8, PlainChar),
    ("SCHAR", 8, Signed),
    ("UCHAR", 8, PromotedUnsigned),
    ("SHRT", c_short::BITS, Signed),
    ("USHRT", c_short::BITS, PromotedUnsigned),
    ("INT", c_int::BITS, Signed),
    ("UINT", c_int::BITS, PromotedUnsigned),
    ("LONG", c_long::BITS, Signed),
    ("ULONG", c_long::BITS, PromotedUnsigned),
    ("LLONG", c_longlong::BITS, Signed),
    ("ULLONG", c_longlong::BITS, PromotedUnsigned),
];

const STDINT: &[(&str, u32, Representation)] = &[
    ("INT8", 8, Signed),
    ("UINT8", 8, PromotedUnsigned),
    ("INT16", 16, Signed),
    ("UINT16", 16, PromotedUnsigned),
    ("INT32", 32, Signed),
    ("UINT32", 32, PromotedUnsigned),
    ("INT64", 64, Signed),
    ("UINT64", 64, PromotedUnsigned),
    ("PTRDIFF", usize::BITS, Signed),
    ("INTPTR", usize::BITS, Signed),
    ("UINTPTR", usize::BITS, PromotedUnsigned),
    ("INTMAX", 64, Signed),
    ("UINTMAX", 64, PromotedUnsigned),
    ("SIZE", usize::BITS, Unsigned),
];

fn bound(
    families: &[(&str, u32, Representation)],
    name: &str,
    unsigned_char: Option<bool>,
) -> Option<Integer> {
    let (family, end) = name.rsplit_once('_')?;
    let &(_, bits, representation) = families.iter().find(|row| row.0 == family)?;
    let mask = u64::MAX.checked_shr(64u32.checked_sub(bits)?)?;
    let unsigned_char = unsigned_char.unwrap_or(c_char::MIN == 0);
    match end {
        "MIN" => representation.minimum(mask, unsigned_char),
        "MAX" => Some(representation.maximum(mask, bits, unsigned_char)),
        _ => None,
    }
}

pub(super) fn limits(name: &str, unsigned_char: Option<bool>) -> Option<Integer> {
    if name == "CHAR_BIT" {
        return Some(Integer::signed(8));
    }
    bound(LIMITS, name, unsigned_char)
}

pub(super) fn stdint(name: &str) -> Option<Integer> {
    bound(STDINT, name, None)
}

#[derive(PartialEq, Eq)]
pub(super) enum Header {
    Limits,
    Stdint,
    Other,
}

// ISO C/C++ library headers have reserved macro namespaces. Arbitrary missing
// vendor and project headers can affect the entire application namespace.
pub(super) fn header(s: &str) -> Option<Header> {
    match s {
        "limits.h" | "climits" => Some(Header::Limits),
        "stdint.h" | "cstdint" | "inttypes.h" | "cinttypes" => Some(Header::Stdint),
        "sys/stat.h" | "sys/types.h" | "fcntl.h" | "unistd.h" | "assert.h" | "complex.h"
        | "ctype.h" | "errno.h" | "fenv.h" | "float.h" | "iso646.h" | "locale.h" | "math.h"
        | "setjmp.h" | "signal.h" | "stdalign.h" | "stdarg.h" | "stdatomic.h" | "stdbool.h"
        | "stddef.h" | "stdio.h" | "stdlib.h" | "stdnoreturn.h" | "string.h" | "tgmath.h"
        | "threads.h" | "time.h" | "uchar.h" | "wchar.h" | "wctype.h" | "algorithm" | "any"
        | "array" | "atomic" | "bit" | "bitset" | "cassert" | "cctype" | "cerrno" | "cfenv"
        | "cfloat" | "charconv" | "chrono" | "clocale" | "cmath" | "codecvt" | "compare"
        | "complex" | "concepts" | "condition_variable" | "coroutine" | "csetjmp" | "csignal"
        | "cstdarg" | "cstddef" | "cstdio" | "cstdlib" | "cstring" | "ctime" | "cwchar"
        | "cwctype" | "deque" | "exception" | "execution" | "filesystem" | "format"
        | "forward_list" | "fstream" | "functional" | "future" | "initializer_list" | "iomanip"
        | "ios" | "iosfwd" | "iostream" | "istream" | "iterator" | "limits" | "list" | "locale"
        | "map" | "memory" | "memory_resource" | "mutex" | "new" | "numbers" | "numeric"
        | "optional" | "ostream" | "queue" | "random" | "ranges" | "ratio" | "regex" | "set"
        | "shared_mutex" | "span" | "sstream" | "stack" | "stdexcept" | "streambuf" | "string"
        | "string_view" | "system_error" | "thread" | "tuple" | "type_traits" | "typeindex"
        | "typeinfo" | "unordered_map" | "unordered_set" | "utility" | "valarray" | "variant"
        | "vector" | "version" => Some(Header::Other),
        _ => None,
    }
}
