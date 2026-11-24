//! A small native ABI model for standard integer headers. The rest of the
//! standard macro namespace is left unknown when its defining text is absent.
use super::condition::Integer;
use std::os::raw::{c_char, c_int, c_long, c_longlong, c_short};

fn signed_limit(bits: u32, minimum: bool) -> Option<Integer> {
    let n = if minimum {
        -(1i128 << (bits - 1))
    } else {
        (1i128 << (bits - 1)) - 1
    };
    i64::try_from(n).ok().map(Integer::signed)
}
fn bound(bits: u32, signed: bool, minimum: bool) -> Option<Integer> {
    if signed {
        signed_limit(bits, minimum)
    } else {
        Some(if minimum {
            Integer::signed(0)
        } else {
            let max = u64::MAX >> (64 - bits);
            if bits < c_int::BITS {
                Integer::signed(max as i64)
            } else {
                Integer::unsigned(max)
            }
        })
    }
}
pub(super) fn limits(name: &str, unsigned_char: Option<bool>) -> Option<Integer> {
    if name == "CHAR_BIT" {
        return Some(Integer::signed(8));
    }
    let (ty, min) = if let Some(ty) = name.strip_suffix("_MAX") {
        (ty, false)
    } else {
        (name.strip_suffix("_MIN")?, true)
    };
    let (bits, signed) = match ty {
        "CHAR" => (8, !unsigned_char.unwrap_or(c_char::MIN == 0)),
        "SCHAR" => (8, true),
        "UCHAR" => (8, false),
        "SHRT" => (c_short::BITS, true),
        "USHRT" => (c_short::BITS, false),
        "INT" => (c_int::BITS, true),
        "UINT" => (c_int::BITS, false),
        "LONG" => (c_long::BITS, true),
        "ULONG" => (c_long::BITS, false),
        "LLONG" => (c_longlong::BITS, true),
        "ULLONG" => (c_longlong::BITS, false),
        _ => return None,
    };
    if !signed && min && ty != "CHAR" {
        return None;
    }
    bound(bits, signed, min)
}
pub(super) fn stdint(name: &str) -> Option<Integer> {
    let (ty, min) = if let Some(ty) = name.strip_suffix("_MAX") {
        (ty, false)
    } else {
        (name.strip_suffix("_MIN")?, true)
    };
    let signed = !ty.starts_with('U');
    let base = ty.strip_prefix('U').unwrap_or(ty);
    let bits = match base {
        "SIZE" if signed && !min => return Some(Integer::unsigned(usize::MAX as u64)),
        "PTRDIFF" if signed => usize::BITS,
        "INTPTR" => usize::BITS,
        "INTMAX" => 64,
        _ => match base.strip_prefix("INT")? {
            "8" => 8,
            "16" => 16,
            "32" => 32,
            "64" => 64,
            _ => return None,
        },
    };
    if !signed && min {
        return None;
    }
    bound(bits, signed, min)
}

// ISO C/C++ library headers have reserved macro namespaces. Arbitrary missing
// vendor and project headers can affect the entire application namespace.
pub(super) fn standard_header(s: &str) -> bool {
    matches!(
        s,
        "sys/stat.h"
            | "sys/types.h"
            | "fcntl.h"
            | "unistd.h"
            | "assert.h"
            | "complex.h"
            | "ctype.h"
            | "errno.h"
            | "fenv.h"
            | "float.h"
            | "inttypes.h"
            | "iso646.h"
            | "limits.h"
            | "locale.h"
            | "math.h"
            | "setjmp.h"
            | "signal.h"
            | "stdalign.h"
            | "stdarg.h"
            | "stdatomic.h"
            | "stdbool.h"
            | "stddef.h"
            | "stdint.h"
            | "stdio.h"
            | "stdlib.h"
            | "stdnoreturn.h"
            | "string.h"
            | "tgmath.h"
            | "threads.h"
            | "time.h"
            | "uchar.h"
            | "wchar.h"
            | "wctype.h"
            | "algorithm"
            | "any"
            | "array"
            | "atomic"
            | "bit"
            | "bitset"
            | "cassert"
            | "cctype"
            | "cerrno"
            | "cfenv"
            | "cfloat"
            | "charconv"
            | "chrono"
            | "cinttypes"
            | "climits"
            | "clocale"
            | "cmath"
            | "codecvt"
            | "compare"
            | "complex"
            | "concepts"
            | "condition_variable"
            | "coroutine"
            | "csetjmp"
            | "csignal"
            | "cstdarg"
            | "cstddef"
            | "cstdint"
            | "cstdio"
            | "cstdlib"
            | "cstring"
            | "ctime"
            | "cwchar"
            | "cwctype"
            | "deque"
            | "exception"
            | "execution"
            | "filesystem"
            | "format"
            | "forward_list"
            | "fstream"
            | "functional"
            | "future"
            | "initializer_list"
            | "iomanip"
            | "ios"
            | "iosfwd"
            | "iostream"
            | "istream"
            | "iterator"
            | "limits"
            | "list"
            | "locale"
            | "map"
            | "memory"
            | "memory_resource"
            | "mutex"
            | "new"
            | "numbers"
            | "numeric"
            | "optional"
            | "ostream"
            | "queue"
            | "random"
            | "ranges"
            | "ratio"
            | "regex"
            | "set"
            | "shared_mutex"
            | "span"
            | "sstream"
            | "stack"
            | "stdexcept"
            | "streambuf"
            | "string"
            | "string_view"
            | "system_error"
            | "thread"
            | "tuple"
            | "type_traits"
            | "typeindex"
            | "typeinfo"
            | "unordered_map"
            | "unordered_set"
            | "utility"
            | "valarray"
            | "variant"
            | "vector"
            | "version"
    )
}
