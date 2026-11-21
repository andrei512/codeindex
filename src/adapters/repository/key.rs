use std::{os::unix::ffi::OsStrExt, path::Path};

// The store namespace hashes canonical root bytes without Git object framing.
pub fn cache_key(root: &Path) -> String {
    super::hex(&super::digest::digest(b"", root.as_os_str().as_bytes(), true).bytes)
}
#[test]
fn known_vector() {
    assert_eq!(
        cache_key(Path::new("abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    )
}
