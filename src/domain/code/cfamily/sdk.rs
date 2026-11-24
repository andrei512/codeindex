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
                Ok(dir) if dir.is_dir() => {
                    if fs::symlink_metadata(child)
                        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
                    {
                        return Self {
                            path: parent.to_string_lossy().into_owned(),
                            stamp: Some(stamp(&dir)),
                        };
                    }
                    break;
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

// Only system-managed roots. A command line can select these roots but cannot
// grant the indexer access to arbitrary external directories.
pub(super) fn trusted(path: &Path) -> bool {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    #[cfg(test)]
    if let Some(root) = std::env::var_os("CX_TEST_TRUSTED_SDK_ROOT") {
        let root = Path::new(&root);
        if root.is_absolute()
            && root.components().all(|c| c != Component::ParentDir)
            && (path.starts_with(root)
                || root.canonicalize().is_ok_and(|real| path.starts_with(real)))
        {
            return true;
        }
    }
    if cfg!(target_os = "macos") {
        return [
            "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr",
            "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs",
            "/Library/Developer/CommandLineTools/SDKs",
            "/Library/Developer/CommandLineTools/usr/lib/clang",
            "/Library/Developer/CommandLineTools/usr/include",
        ]
        .iter()
        .any(|root| path.starts_with(root));
    }
    path.starts_with("/usr/include")
        || path.starts_with("/usr/lib/clang")
        || path.starts_with("/usr/lib/gcc")
        || path
            .strip_prefix("/usr/lib")
            .ok()
            .and_then(|rest| rest.components().next())
            .is_some_and(|part| {
                part.as_os_str().to_str().is_some_and(|s| {
                    s.strip_prefix("llvm-")
                        .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                })
            })
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
    includes: Vec<(Include, String, bool)>,
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

// A newline inside a block comment disappears when the preprocessor
// replaces the comment by one space. The lexer preserves physical positions.
fn starts_new_line(mut gap: &str) -> bool {
    while let Some((before, comment)) = gap.split_once("/*") {
        if before.contains('\n') || before.contains("//") && gap.contains('\n') {
            return true;
        }
        let Some((_, after)) = comment.split_once("*/") else {
            return true;
        };
        gap = after;
    }
    gap.contains('\n')
}

impl Header {
    fn parse(text: &str) -> Option<Self> {
        // Line splicing precedes tokenization. Reject alternate encodings we
        // cannot account for, including trigraphs and universal identifier escapes.
        if text
            .as_bytes()
            .windows(3)
            .any(|w| &w[..2] == b"??" && b"=/()!<>-'".contains(&w[2]))
        {
            return None;
        }
        let unix;
        let text = if text.contains('\r') {
            unix = text.replace("\r\n", "\n").replace('\r', "\n");
            unix.as_str()
        } else {
            text
        };
        let logical;
        fn splice(s: &str) -> Option<&str> {
            s.strip_suffix('\n')?
                .trim_end_matches([' ', '\t'])
                .strip_suffix('\\')
        }
        let text = if text.contains('\\') && text.split_inclusive('\n').any(|s| splice(s).is_some())
        {
            let mut joined = String::with_capacity(text.len());
            for line in text.split_inclusive('\n') {
                joined.push_str(splice(line).unwrap_or(line));
            }
            logical = joined;
            logical.as_str()
        } else {
            text
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        // Typical C++ headers contain ordinary code and // comments. For these
        // files all possible preprocessing directives begin on their own line.
        // Keep the full lexer whenever literal or comment scope spans lines.
        let filtered;
        let text = if !text.contains("/*") && !text.contains("R\"") && !text.contains("_Pragma") {
            filtered = text
                .split_inclusive('\n')
                .filter(|line| {
                    let line = line.trim_start();
                    line.starts_with('#') || line.starts_with("%:")
                })
                .collect::<String>();
            filtered.as_str()
        } else {
            text
        };
        let tokens = lexer::tokens(text, Language::Cpp);
        // Escaped identifiers can spell an ASCII macro in some dialects.
        // Escapes in character/string literals are unrelated to macro names.
        if tokens.iter().any(|t| t.text == "\\") {
            return None;
        }
        let mut conditions = Vec::new();
        let mut out = Self::default();
        // Pragma operators may be embedded in macro replacement lists.
        // A literal pop restores a previously saved definition.
        for token in &tokens {
            if token.kind == lexer::Kind::String && token.text.contains("pop_macro") {
                let body = token.text.trim_matches('"').trim();
                if let Some(name) = body
                    .strip_prefix("pop_macro")
                    .and_then(|s| s.trim().strip_prefix('('))
                    .and_then(|s| s.trim().strip_suffix(')'))
                    .and_then(|s| s.trim().strip_prefix(r#"\""#))
                    .and_then(|s| s.strip_suffix(r#"\""#))
                {
                    out.writes.insert(name.to_owned());
                } else {
                    return None;
                }
            }
        }

        let mut i = 0;
        while i < tokens.len() {
            let mut end = i + 1;
            while end < tokens.len()
                && (tokens[end].line == tokens[end - 1].line
                    || !starts_new_line(&text[tokens[end - 1].span.end..tokens[end].span.start]))
            {
                end += 1;
            }
            let row = &tokens[i..end];
            i = end;
            let skip = match row[0].text {
                "#" => 1,
                "%" if row.get(1).is_some_and(|t| t.text == ":") => 2,
                _ => continue,
            };
            let Some(command) = row.get(skip) else {
                continue;
            };
            let args = &row[skip + 1..];
            match command.text {
                // A conventional outer #ifndef is an include guard. Its body
                // alone does not make a missing header optional.
                "if" | "ifdef" | "ifndef" => {
                    conditions.push(command.text != "ifndef" || !conditions.is_empty())
                }
                "else" => {
                    *conditions.last_mut()? = true;
                }
                "elif" | "elifdef" | "elifndef" => {
                    *conditions.last_mut()? = true;
                }
                "endif" => {
                    conditions.pop()?;
                }
                "define" | "undef" => {
                    let name = args.first()?.text;
                    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                        return None;
                    }
                    out.writes.insert(name.to_owned());
                }
                "include" | "import" | "include_next" => {
                    let first = args.first()?;
                    let (kind, name) = if let Some(name) = first
                        .text
                        .strip_prefix('"')
                        .and_then(|x| x.strip_suffix('"'))
                    {
                        if args.len() != 1 {
                            return None;
                        }
                        (Include::Quote, name)
                    } else {
                        let last = args.last()?;
                        if first.text != "<" || last.text != ">" {
                            return None;
                        }
                        (Include::Angle, text.get(first.span.end..last.span.start)?)
                    };
                    if !literal_name(name) {
                        return None;
                    }
                    out.includes.push((
                        if command.text == "include_next" {
                            Include::Next
                        } else {
                            kind
                        },
                        name.to_owned(),
                        conditions.contains(&true),
                    ));
                }
                "pragma"
                    if args
                        .first()
                        .is_some_and(|t| matches!(t.text, "push_macro" | "pop_macro")) =>
                {
                    let name = args.get(2)?.text.strip_prefix('"')?.strip_suffix('"')?;
                    out.writes.insert(name.to_owned());
                }
                _ => {}
            }
        }
        conditions.is_empty().then_some(out)
    }
}

struct Candidate {
    receipt: Option<Receipt>,
    result: Result<Option<Header>, ()>,
}
impl Candidate {
    fn read(path: &Path) -> Self {
        let unknown = || Self {
            receipt: None,
            result: Err(()),
        };
        if !trusted(path) {
            return unknown();
        }
        let meta = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Self {
                    receipt: Some(Receipt::missing(path)),
                    result: Ok(None),
                };
            }
            _ => return unknown(),
        };
        let canonical = match fs::canonicalize(path) {
            Ok(p) if trusted(&p) => p,
            _ => return unknown(),
        };
        let path_name = path.to_string_lossy().into_owned();
        let receipt = Receipt {
            path: path_name,
            stamp: Some(stamp(&meta)),
        };
        let result = (|| {
            if !meta.is_file() || meta.len() > 4 << 20 {
                return Err(());
            }
            let mut file = File::open(canonical).map_err(|_| ())?;
            if stamp(&file.metadata().map_err(|_| ())?) != stamp(&meta) {
                return Err(());
            }
            let mut body = String::with_capacity(meta.len() as usize);
            (&mut file)
                .take((4 << 20) + 1)
                .read_to_string(&mut body)
                .map_err(|_| ())?;
            if body.len() > 4 << 20 || stamp(&file.metadata().map_err(|_| ())?) != stamp(&meta) {
                return Err(());
            }
            Header::parse(&body).map(Some).ok_or(())
        })();
        Self {
            receipt: Some(receipt),
            result,
        }
    }
}

// Only the explicitly witnessed Apple Clang layout has a closed native
// search. Other toolchains can still use a fully present literal include graph.
struct NativeSearch<'a> {
    sdk: &'a Path,
    toolchain: &'a Path,
}

impl<'a> NativeSearch<'a> {
    fn from_dirs(dirs: &'a [String]) -> Option<Self> {
        if !cfg!(target_vendor = "apple") {
            return None;
        }
        let paths = || dirs.iter().map(Path::new);
        let sdk = paths()
            .find(|p| {
                p.ends_with("usr/include")
                    && p.parent()
                        .and_then(Path::parent)
                        .is_some_and(|s| s.extension().is_some_and(|e| e == "sdk") && trusted(s))
            })?
            .parent()?
            .parent()?;
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
            .then_some(Self { sdk, toolchain })
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
    files: Mutex<HashMap<PathBuf, Slot<Arc<Candidate>>>>,
    scans: Mutex<HashMap<Vec<String>, HashMap<String, [Slot<ResultSet>; 2]>>>,
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
    ) -> Option<Arc<Proof>> {
        self.slot(dirs, header, optional)
            .get_or_init(|| self.scan(dirs, header, optional))
            .proof
            .clone()
    }
    pub(super) fn receipts_for(
        &self,
        dirs: &[String],
        header: &str,
        optional: bool,
    ) -> Vec<Receipt> {
        self.slot(dirs, header, optional)
            .get_or_init(|| self.scan(dirs, header, optional))
            .receipts
            .clone()
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
    fn scan(&self, dirs: &[String], name: &str, optional: bool) -> ResultSet {
        let mut receipts = BTreeSet::new();
        let mut seen = HashSet::new();
        let mut nested = BTreeSet::new();
        let mut writes = HashSet::new();
        let mut requests = HashSet::new();
        let proof = (|| {
            if dirs.is_empty() || !literal_name(name) {
                return None;
            }
            let native = NativeSearch::from_dirs(dirs);
            let mut checked_fallbacks = false;
            let mut frameworks = HashSet::new();
            let mut todo = vec![(name.to_owned(), None, 0, optional)];
            while let Some(request) = todo.pop() {
                if !requests.insert(request.clone()) {
                    continue;
                }
                let (name, local, start, optional) = request;
                let mut found = false;
                let candidates = local.into_iter().map(|dir| (dir, start)).chain(
                    dirs.iter()
                        .enumerate()
                        .skip(start)
                        .map(|(n, s)| (PathBuf::from(s), n + 1)),
                );
                for (dir, next) in candidates {
                    let path = dir.join(&name);
                    if !trusted(&path) {
                        return None;
                    }
                    let slot = self
                        .files
                        .lock()
                        .unwrap()
                        .entry(path.clone())
                        .or_default()
                        .clone();
                    let c = slot
                        .get_or_init(|| Arc::new(Candidate::read(&path)))
                        .clone();
                    receipts.insert(c.receipt.as_ref()?.clone());
                    if receipts.len() > 32768 {
                        return None;
                    }
                    if let Some(h) = c.result.as_ref().ok()? {
                        found = true;
                        if seen.insert((path.clone(), next, optional)) {
                            if seen.len() > 4096 {
                                return None;
                            }
                            for (kind, child, conditional) in &h.includes {
                                nested.insert(child.clone());
                                let (local, start) = match kind {
                                    Include::Quote => (path.parent().map(Path::to_path_buf), 0),
                                    Include::Angle => (None, 0),
                                    Include::Next => (None, next),
                                };
                                todo.push((child.clone(), local, start, optional || *conditional));
                            }
                            for name in &h.writes {
                                if !writes.contains(name) {
                                    writes.insert(name.clone());
                                }
                            }
                        }
                        break;
                    }
                }
                if !found {
                    if !optional {
                        return None;
                    }
                    let native = native.as_ref()?;
                    if !checked_fallbacks {
                        native.check_fallbacks(&mut receipts)?;
                        checked_fallbacks = true;
                    }
                    if let Some((framework, _)) = name.split_once('/') {
                        if frameworks.insert(framework.to_owned()) {
                            native.check_framework(framework, &mut receipts)?;
                        }
                    }
                }
            }
            Some(Arc::new(Proof {
                writes,
                nested: nested.into_iter().collect(),
            }))
        })();
        ResultSet {
            proof,
            receipts: receipts.into_iter().collect(),
        }
    }
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
        assert!(!h.includes[0].2 && h.includes[1].2);
        let h = Header::parse(r#"#define RESTORE _Pragma("pop_macro(\"outside\")")"#).unwrap();
        assert!(h.writes.contains("outside"));
    }
}
