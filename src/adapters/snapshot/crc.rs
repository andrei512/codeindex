const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < table.len() {
        let mut v = i as u32;
        let mut bit = 0;
        while bit < 8 {
            v = (v >> 1) ^ (0u32.wrapping_sub(v & 1) & 0x82f63b78);
            bit += 1;
        }
        table[i] = v;
        i += 1;
    }
    table
};

pub fn crc32c(bytes: &[u8]) -> u32 {
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("crc") {
        // SAFETY: the runtime feature check covers all instructions in arm_crc.
        return unsafe { arm_crc(bytes) };
    }
    let mut crc = u32::MAX;
    for b in bytes {
        crc = TABLE[((crc as u8) ^ b) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "crc")]
unsafe fn arm_crc(bytes: &[u8]) -> u32 {
    use std::arch::aarch64::{__crc32cb, __crc32cd};
    let mut crc = u32::MAX;
    let (words, tail) = bytes.as_chunks::<8>();
    for word in words {
        crc = __crc32cd(crc, u64::from_le_bytes(*word));
    }
    for &b in tail {
        crc = __crc32cb(crc, b);
    }
    !crc
}

#[test]
fn castagnoli_vector() {
    assert_eq!(crc32c(b"123456789"), 0xe3069283);
}
