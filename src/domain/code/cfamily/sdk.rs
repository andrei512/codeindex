//! Negative macro evidence from explicitly configured native SDK include directories.
//! Follow literal includes only. Every conditional branch contributes possible writes.
use super::super::{
    Language,
    cache::{self, Wire},
    lexer,
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File, Metadata},
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Receipt {
    path: String,
    stamp: Option<[u64; 7]>,
}

fn stamp(meta: &Metadata) -> [u64; 7] {
    [
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime() as u64,
        meta.mtime_nsec() as u64,
        meta.ctime() as u64,
        meta.ctime_nsec() as u64,
    ]
}

impl Receipt {
    #[cfg(test)]
    pub(crate) fn test_versions() -> (Self, Self) {
        let base = if cfg!(target_os = "macos") {
            "/Library/Developer/CommandLineTools/SDKs"
        } else {
            "/usr/include"
        };
        let path = format!("{base}/.cx-unobserved-test-{}", std::process::id());
        (
            Self {
                path: path.clone(),
                stamp: Some([0; 7]),
            },
            Self { path, stamp: None },
        )
    }

    // A directory stamp can witness many missing child names. Capture the
    // directory first, then confirm the immediate child is absent. A dangling
    // symlink may become live without a parent mutation: retain a leaf receipt.
    fn missing(path: &Path) -> Self {
        let mut child = path;
        while let Some(parent) = child.parent().filter(|p| trusted(p)) {
            match fs::metadata(parent) {
                Ok(dir)
                    if dir.is_dir()
                        && fs::symlink_metadata(child)
                            .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
                {
                    return Self {
                        path: parent.to_string_lossy().into_owned(),
                        stamp: Some(stamp(&dir)),
                    };
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => child = parent,
                _ => break,
            }
        }
        Self {
            path: path.to_string_lossy().into_owned(),
            stamp: None,
        }
    }
    pub(crate) fn key(&self) -> &str {
        &self.path
    }
    pub(crate) fn valid(&self) -> bool {
        let path = Path::new(&self.path);
        if !receipt_path(path) {
            return false;
        }
        match fs::metadata(path) {
            Ok(meta) => self.stamp == Some(stamp(&meta)),
            Err(e) => self.stamp.is_none() && e.kind() == io::ErrorKind::NotFound,
        }
    }
}

impl Wire for Receipt {
    fn save(&self, w: &mut impl cache::Output) {
        self.path.save(w);
        self.stamp.is_some().save(w);
        if let Some(stamp) = self.stamp {
            for word in stamp {
                w.extend_from_slice(&word.to_le_bytes());
            }
        }
    }
    fn load(r: &mut &[u8]) -> io::Result<Self> {
        let path = String::load(r)?;
        if !receipt_path(Path::new(&path)) {
            return Err(cache::bad());
        }
        let stamp = if bool::load(r)? {
            let mut words = [0; 7];
            for word in &mut words {
                let (bytes, rest) = r.split_at_checked(8).ok_or_else(cache::bad)?;
                *word = u64::from_le_bytes(bytes.try_into().unwrap());
                *r = rest;
            }
            Some(words)
        } else {
            None
        };
        Ok(Self { path, stamp })
    }
}

// The Apple driver also looks for an optional local include directory.
// We may witness its absence, never read files from that root.
fn receipt_path(path: &Path) -> bool {
    trusted(path) || cfg!(target_vendor = "apple") && path == Path::new("/usr/local/include")
}

fn absolute_without_parent(path: &Path) -> bool {
    path.is_absolute() && path.components().all(|part| part != Component::ParentDir)
}

#[cfg(target_os = "macos")]
const TRUSTED_ROOTS: &[&str] = &[
    "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr",
    "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs",
    "/Library/Developer/CommandLineTools/SDKs",
    "/Library/Developer/CommandLineTools/usr/lib/clang",
    "/Library/Developer/CommandLineTools/usr/include",
];

#[cfg(not(target_os = "macos"))]
const TRUSTED_ROOTS: &[&str] = &["/usr/include", "/usr/lib/clang", "/usr/lib/gcc"];

#[cfg(not(target_os = "macos"))]
fn trusted_llvm(path: &Path) -> bool {
    path.strip_prefix("/usr/lib")
        .ok()
        .and_then(|rest| rest.components().next())
        .and_then(|part| part.as_os_str().to_str())
        .and_then(|part| part.strip_prefix("llvm-"))
        .is_some_and(|version| !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
fn trusted_test_root(path: &Path) -> bool {
    let Some(root) = std::env::var_os("CX_TEST_TRUSTED_SDK_ROOT") else {
        return false;
    };
    let root = Path::new(&root);
    absolute_without_parent(root)
        && (path.starts_with(root) || root.canonicalize().is_ok_and(|real| path.starts_with(real)))
}

// Only system-managed roots. A command line can select these roots but cannot
// grant the indexer access to arbitrary external directories.
pub(super) fn trusted(path: &Path) -> bool {
    if !absolute_without_parent(path) {
        return false;
    }
    #[cfg(test)]
    if trusted_test_root(path) {
        return true;
    }
    #[cfg(not(target_os = "macos"))]
    if trusted_llvm(path) {
        return true;
    }
    TRUSTED_ROOTS.iter().any(|root| path.starts_with(root))
}

#[derive(Clone, Copy)]
enum Include {
    Quote,
    Angle,
    Next,
}
#[derive(Default)]
struct Header {
    writes: HashSet<String>,
    includes: Vec<HeaderInclude>,
}

struct HeaderInclude {
    kind: Include,
    name: String,
    conditional: bool,
}

#[derive(Hash, PartialEq, Eq)]
struct HeaderRequest {
    name: String,
    local: Option<PathBuf>,
    start: usize,
    optional: bool,
}

impl HeaderRequest {
    fn prove_missing<'d>(
        &self,
        dirs: &'d [String],
        native: &mut Option<Option<NativeSearch<'d>>>,
        receipts: &mut BTreeSet<Receipt>,
    ) -> Option<()> {
        if !self.optional {
            return None;
        }
        native
            .get_or_insert_with(|| NativeSearch::from_dirs(dirs))
            .as_mut()?
            .prove_missing(&self.name, receipts)
    }

    fn entry(name: &str, optional: bool) -> Self {
        Self {
            name: name.to_owned(),
            local: None,
            start: 0,
            optional,
        }
    }

    fn directories<'a>(&'a self, dirs: &'a [String]) -> impl Iterator<Item = (&'a Path, usize)> {
        self.local
            .as_deref()
            .into_iter()
            .map(|dir| (dir, self.start))
            .chain(
                dirs.iter()
                    .enumerate()
                    .skip(self.start)
                    .map(|(index, dir)| (Path::new(dir), index + 1)),
            )
    }
}

impl HeaderInclude {
    fn request(&self, path: &Path, next: usize, optional: bool) -> HeaderRequest {
        let mut request = HeaderRequest::entry(&self.name, optional || self.conditional);
        match self.kind {
            Include::Quote => request.local = path.parent().map(Path::to_path_buf),
            Include::Next => request.start = next,
            Include::Angle => {}
        }
        request
    }
}

fn literal_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 4096
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(&b))
        && Path::new(s)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

mod header;

struct Candidate {
    receipt: Option<Receipt>,
    result: Result<Option<Header>, ()>,
}
struct TrustedHeader {
    canonical: PathBuf,
    metadata: Metadata,
    observed: [u64; 7],
}

impl TrustedHeader {
    fn observe(path: &Path) -> Result<Option<Self>, ()> {
        let metadata = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(()),
        };
        let canonical = fs::canonicalize(path).map_err(|_| ())?;
        if !trusted(&canonical) {
            return Err(());
        }
        let observed = stamp(&metadata);
        Ok(Some(Self {
            canonical,
            metadata,
            observed,
        }))
    }

    fn read(self) -> Result<Header, ()> {
        let Self {
            canonical,
            metadata,
            observed,
        } = self;
        if !metadata.is_file() || metadata.len() > 4 << 20 {
            return Err(());
        }
        let mut file = File::open(canonical).map_err(|_| ())?;
        if stamp(&file.metadata().map_err(|_| ())?) != observed {
            return Err(());
        }
        let mut body = String::with_capacity(metadata.len() as usize);
        (&mut file)
            .take((4 << 20) + 1)
            .read_to_string(&mut body)
            .map_err(|_| ())?;
        if body.len() > 4 << 20 || stamp(&file.metadata().map_err(|_| ())?) != observed {
            return Err(());
        }
        Header::parse(&body).ok_or(())
    }
}

impl Candidate {
    fn read_trusted(path: &Path) -> Self {
        match TrustedHeader::observe(path) {
            Ok(Some(header)) => Self {
                receipt: Some(Receipt {
                    path: path.to_string_lossy().into_owned(),
                    stamp: Some(header.observed),
                }),
                result: header.read().map(Some),
            },
            Ok(None) => Self {
                receipt: Some(Receipt::missing(path)),
                result: Ok(None),
            },
            Err(()) => Self {
                receipt: None,
                result: Err(()),
            },
        }
    }
}

// Only the explicitly witnessed Apple Clang layout has a closed native
// search. Other toolchains can still use a fully present literal include graph.
struct NativeSearch<'a> {
    sdk: &'a Path,
    toolchain: &'a Path,
    checked_fallbacks: bool,
    frameworks: HashSet<String>,
}

pub(super) fn sdk_include_root(include: &Path) -> Option<&Path> {
    let root = include.parent()?.parent()?;
    (include.ends_with("usr/include") && root.extension().is_some_and(|e| e == "sdk"))
        .then_some(root)
}

impl<'a> NativeSearch<'a> {
    fn from_dirs(dirs: &'a [String]) -> Option<Self> {
        if !cfg!(target_vendor = "apple") {
            return None;
        }
        let paths = || dirs.iter().map(Path::new);
        let sdk = paths()
            .filter_map(sdk_include_root)
            .find(|root| trusted(root))?;
        let toolchain = paths()
            .find(|p| p.ends_with("XcodeDefault.xctoolchain/usr/include") && trusted(p))?
            .parent()?;
        let resource = toolchain.join("lib/clang");
        paths()
            .any(|p| {
                p.starts_with(&resource)
                    && p.ends_with("include")
                    && p.parent().and_then(Path::parent) == Some(&resource)
            })
            .then_some(Self {
                sdk,
                toolchain,
                checked_fallbacks: false,
                frameworks: HashSet::new(),
            })
    }
    fn absent(path: &Path, receipts: &mut BTreeSet<Receipt>) -> Option<()> {
        if !receipt_path(path) {
            return None;
        }
        let stamp = match fs::metadata(path) {
            Ok(m) => Some(stamp(&m)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            _ => return None,
        };
        receipts.insert(Receipt {
            path: path.to_str()?.to_owned(),
            stamp,
        });
        stamp.is_none().then_some(())
    }
    fn prove_missing(&mut self, name: &str, receipts: &mut BTreeSet<Receipt>) -> Option<()> {
        if !self.checked_fallbacks {
            self.check_fallbacks(receipts)?;
            self.checked_fallbacks = true;
        }
        if let Some((framework, _)) = name.split_once('/')
            && self.frameworks.insert(framework.to_owned())
        {
            self.check_framework(framework, receipts)?;
        }
        Some(())
    }
    fn check_fallbacks(&self, receipts: &mut BTreeSet<Receipt>) -> Option<()> {
        Self::absent(Path::new("/usr/local/include"), receipts)?;
        Self::absent(&self.sdk.join("usr/local/include"), receipts)?;
        Self::absent(&self.toolchain.join("include/c++/v1"), receipts)
    }
    fn check_framework(&self, name: &str, receipts: &mut BTreeSet<Receipt>) -> Option<()> {
        for dir in [
            "System/Library/Frameworks",
            "System/Library/SubFrameworks",
            "Library/Frameworks",
        ] {
            Self::absent(
                &self.sdk.join(dir).join(format!("{name}.framework")),
                receipts,
            )?;
        }
        Some(())
    }
}

#[derive(Default)]
pub(super) struct Proof {
    writes: HashSet<String>,
    nested: Vec<String>,
}
impl Proof {
    pub(super) fn may_modify(&self, name: &str) -> bool {
        self.writes.contains(name)
    }
    pub(super) fn nested_headers(&self) -> &[String] {
        &self.nested
    }
}
struct ResultSet {
    proof: Option<Arc<Proof>>,
    receipts: Vec<Receipt>,
}
type Slot<T> = Arc<OnceLock<T>>;
#[derive(Default)]
pub(super) struct Sdk {
    files: Mutex<HashMap<PathBuf, Slot<Candidate>>>,
    scans: Mutex<HashMap<Vec<String>, HashMap<String, [Slot<ResultSet>; 2]>>>,
}
/// The header traversal tracks each inclusion context separately. The same
/// path can resume #include_next at a different directory or be optional.
#[derive(Default)]
struct IncludeGraph {
    seen: HashSet<(PathBuf, usize, bool)>,
    nested: BTreeSet<String>,
    writes: HashSet<String>,
    pending: Vec<HeaderRequest>,
}

impl IncludeGraph {
    fn visit(&mut self, path: PathBuf, next: usize, optional: bool, header: &Header) -> Option<()> {
        if !self.seen.insert((path.clone(), next, optional)) {
            return Some(());
        }
        if self.seen.len() > 4096 {
            return None;
        }
        for include in &header.includes {
            self.nested.insert(include.name.clone());
            self.pending.push(include.request(&path, next, optional));
        }
        for name in &header.writes {
            if !self.writes.contains(name) {
                self.writes.insert(name.clone());
            }
        }
        Some(())
    }
}

impl Sdk {
    fn slot(&self, dirs: &[String], header: &str, optional: bool) -> Slot<ResultSet> {
        let mut scans = self.scans.lock().unwrap();
        if let Some(slot) = scans.get(dirs).and_then(|m| m.get(header)) {
            return slot[usize::from(optional)].clone();
        }
        scans
            .entry(dirs.to_vec())
            .or_default()
            .entry(header.into())
            .or_default()[usize::from(optional)]
        .clone()
    }
    pub(super) fn proof(
        &self,
        dirs: &[String],
        header: &str,
        optional: bool,
        consulted: &mut HashSet<String>,
    ) -> Option<Arc<Proof>> {
        let slot = self.slot(dirs, header, optional);
        let scan = slot.get_or_init(|| self.scan(dirs, header, optional));
        // A failed proof still depends on the paths inspected before failure.
        for receipt in &scan.receipts {
            if !consulted.contains(receipt.key()) {
                consulted.insert(receipt.key().to_owned());
            }
        }
        scan.proof.clone()
    }

    pub(super) fn receipts(&self) -> Vec<Receipt> {
        self.scans
            .lock()
            .unwrap()
            .values()
            .flat_map(|headers| headers.values().flatten())
            .filter_map(|s| s.get())
            .flat_map(|s| s.receipts.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn candidate(&self, path: &Path) -> Option<Slot<Candidate>> {
        if !trusted(path) {
            return None;
        }
        let mut files = self.files.lock().unwrap();
        if let Some(slot) = files.get(path) {
            return Some(slot.clone());
        }
        Some(files.entry(path.to_owned()).or_default().clone())
    }

    fn find_header<'a>(
        &self,
        name: &str,
        candidates: impl Iterator<Item = (&'a Path, usize)>,
        receipts: &mut BTreeSet<Receipt>,
        mut visit: impl FnMut(PathBuf, usize, &Header) -> Option<()>,
    ) -> Option<bool> {
        for (dir, next) in candidates {
            let path = dir.join(name);
            let slot = self.candidate(&path)?;
            let candidate = slot.get_or_init(|| Candidate::read_trusted(&path));
            receipts.insert(candidate.receipt.as_ref()?.clone());
            let header = bounded_header(candidate, receipts.len())?;
            let Some(header) = header else { continue };
            visit(path, next, header)?;
            return Some(true);
        }
        Some(false)
    }

    fn scan(&self, dirs: &[String], name: &str, optional: bool) -> ResultSet {
        let mut receipts = BTreeSet::new();
        let proof = self.scan_graph(dirs, name, optional, &mut receipts);
        ResultSet {
            proof,
            receipts: receipts.into_iter().collect(),
        }
    }

    fn scan_graph(
        &self,
        dirs: &[String],
        name: &str,
        optional: bool,
        receipts: &mut BTreeSet<Receipt>,
    ) -> Option<Arc<Proof>> {
        let mut graph = IncludeGraph::default();
        let mut requests = HashMap::new();
        if dirs.is_empty() || !literal_name(name) {
            return None;
        }
        let mut native = None;
        graph.pending.push(HeaderRequest::entry(name, optional));
        while let Some(request) = graph.pending.pop() {
            let std::collections::hash_map::Entry::Vacant(entry) = requests.entry(request) else {
                continue;
            };
            let request = entry.key();
            let name = &request.name;
            let found = self.find_header(
                name,
                request.directories(dirs),
                receipts,
                |path, next, header| graph.visit(path, next, request.optional, header),
            )?;
            found
                .then_some(())
                .or_else(|| request.prove_missing(dirs, &mut native, receipts))?;
            entry.insert(());
        }
        Some(Arc::new(Proof {
            writes: graph.writes,
            nested: graph.nested.into_iter().collect(),
        }))
    }
}

fn bounded_header(candidate: &Candidate, receipts: usize) -> Option<Option<&Header>> {
    if receipts > 32768 {
        return None;
    }
    candidate.result.as_ref().ok().map(Option::as_ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_directives_through_comments_and_splices() {
        let h = Header::parse(
            "\u{feff}# /* one\ntwo */ def\\\nine FIRST 1\n// #define FALSE\n%:undef GONE\n",
        )
        .unwrap();
        assert!(h.writes.contains("FIRST"));
        assert!(h.writes.contains("GONE"));
        assert!(!h.writes.contains("FALSE"));
        assert!(
            Header::parse("int x; // /*\n /* comment */ #define LIVE 1\n")
                .unwrap()
                .writes
                .contains("LIVE")
        );
        assert!(Header::parse("#include COMPUTED\n").is_none());
        assert!(Header::parse("??=define X\n").is_none());
    }

    #[test]
    fn unions_arms_and_distinguishes_guards_from_optional_includes() {
        let h = Header::parse("#ifndef GUARD\n#define GUARD\n#include <required.h>\n#if CONFIG\n#define A 1\n#include <a.h>\n#else\n#undef B\n#endif\n#endif\n").unwrap();
        assert!(h.writes.contains("A") && h.writes.contains("B"));
        assert!(!h.includes[0].conditional && h.includes[1].conditional);
        let h = Header::parse(r#"#define RESTORE _Pragma("pop_macro(\"outside\")")"#).unwrap();
        assert!(h.writes.contains("outside"));
    }
}
