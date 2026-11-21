// Diagnostic executable provenance. Persisted compatibility is declared in
// adapters/snapshot/registry.rs and does not depend on this fingerprint.
use std::{env, fs, path::Path};

fn absorb(state: &mut u64, bytes: impl IntoIterator<Item = u8>) {
    for b in bytes {
        *state = (*state ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
}
fn hash_path(path: &Path, state: &mut u64) {
    if path.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for entry in entries {
            hash_path(&entry, state);
        }
    } else {
        if path.file_name().is_some_and(|name| name == "tests.rs") {
            return;
        }
        let bytes = fs::read(path).unwrap();
        absorb(
            state,
            path.as_os_str()
                .as_encoded_bytes()
                .iter()
                .copied()
                .chain([0])
                .chain((bytes.len() as u64).to_le_bytes())
                .chain(bytes),
        );
    }
}
fn main() {
    let mut hash = 0xcbf29ce484222325;
    let target = env::var("TARGET").unwrap();
    absorb(&mut hash, target.bytes());
    println!("cargo:rustc-env=CX_TARGET={target}");
    for path in ["build.rs", "Cargo.toml", "Cargo.lock", "src"] {
        println!("cargo:rerun-if-changed={path}");
        hash_path(Path::new(path), &mut hash);
    }
    println!("cargo:rustc-env=CX_BUILD_ID={hash:016x}");
    // Required by the legacy manifest writer while older layouts are supported.
    println!("cargo:rustc-env=CODEINDEX_RULES={hash:016x}");
}
