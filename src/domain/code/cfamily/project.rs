//! Compile database entries describe translation units. Includes only consult
//! the repository capture, so an index never runs a compiler or opens host files.
use super::{
    Analysis, Language, Token,
    sdk::{Proof, Receipt, Sdk},
};
use crate::domain::code::lexer;
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Command {
    search: SearchDirs,
    pub forced: Vec<(String, String)>,
    pub standard: Option<i64>,
    pub native: bool,
    pub unsigned_char: Option<bool>,
    pub defines: HashMap<String, Option<String>>,
    pub cpp: bool,
}
type Lookup<'a> = dyn Fn(&str) -> Option<(&'a str, &'a str)> + Sync + 'a;
pub(super) struct PreprocessingSource<'a> {
    pub(super) text: &'a str,
    pub(super) mode: lexer::Mode,
    pub(super) tokens: Vec<Token<'a>>,
    rows: OnceLock<Vec<super::directives::SourceRow>>,
}
impl<'a> PreprocessingSource<'a> {
    fn new(text: &'a str, mode: lexer::Mode) -> Self {
        Self {
            text,
            mode,
            tokens: mode.tokens(text),
            rows: OnceLock::new(),
        }
    }

    // A previous partition may have populated the cache before written extraction.
    pub(super) fn reusable_rows(&self, shared: bool) -> Option<&[super::directives::SourceRow]> {
        if shared {
            Some(self.rows())
        } else {
            self.rows.get().map(Vec::as_slice)
        }
    }
    pub(super) fn rows(&self) -> &[super::directives::SourceRow] {
        self.rows.get_or_init(|| {
            super::directives::logical_rows(self.text, &self.tokens, self.mode).collect()
        })
    }
}
type TokenCache<'a> =
    Mutex<HashMap<(&'a str, lexer::Mode), Arc<OnceLock<Arc<PreprocessingSource<'a>>>>>>;
pub(crate) struct CompileInputs {
    pub paths: HashMap<String, Vec<String>>,
    pub receipts: Vec<Receipt>,
    // Older snapshots contain only path and SDK observations.
    pub commands: Option<HashMap<String, Vec<String>>>,
}
pub(crate) struct CProject<'a> {
    commands: HashMap<String, Vec<Option<Command>>>,
    consulted: Mutex<HashMap<String, Vec<String>>>,
    lookup: Box<Lookup<'a>>,
    headers: TokenCache<'a>,
    sdk: Sdk,
}
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir if !out.pop() => return None,
            Component::ParentDir | Component::CurDir => {}
            part => out.push(part.as_os_str()),
        }
    }
    Some(out)
}
fn inside(root: &Path, dir: &Path, file: &str) -> Option<String> {
    normalize(&dir.join(file))?
        .strip_prefix(root)
        .ok()?
        .to_str()
        .map(str::to_owned)
}

pub(crate) fn compilation_database(path: &str) -> bool {
    path == "compile_commands.json" || path.ends_with("/compile_commands.json")
}

mod context_key;
mod database;

// SDK directories supplied by a compile database can describe the native
// system header search. User-supplied, unobserved directories stay opaque.
#[derive(Default)]
struct DriverOptions<'a> {
    operands: Vec<(Operand, std::borrow::Cow<'a, str>)>,
    unsigned_char: Option<bool>,
    standard: Option<&'a str>,
    sysroots: Vec<PathBuf>,
    target_override: bool,
    replaced_headers: bool,
}

/// Language switches apply to following input operands. Other driver options
/// (including -o and -I) can take values that spell the source path.
struct DriverSource<'a> {
    root: &'a Path,
    dir: &'a database::Directory,
    file: &'a str,
    default: Language,
}

impl DriverSource<'_> {
    fn matches(&self, value: &str) -> bool {
        if Path::new(value).file_name() != Path::new(self.file).file_name() {
            return false;
        }
        let rebased = self.dir.operand(self.root, value);
        let path = rebased.as_deref().unwrap_or(Path::new(value));
        path.to_str()
            .and_then(|p| inside(self.root, &self.dir.path, p))
            .as_deref()
            == Some(self.file)
    }

    fn record_language(&self, mode: &str, selected: &mut Option<Language>) -> Option<()> {
        let dialect = self.language(mode)?;
        // A single-command proof cannot choose between two modes of the same input.
        match selected.replace(dialect) {
            Some(prior) if prior != dialect => None,
            _ => Some(()),
        }
    }

    fn language(&self, mode: &str) -> Option<Language> {
        match mode {
            "none" => Some(self.default),
            "c"
            | "c-header"
            | "cpp-output"
            | "objective-c"
            | "objective-c-header"
            | "objective-c-cpp-output" => Some(Language::C),
            "c++"
            | "c++-header"
            | "c++-cpp-output"
            | "objective-c++"
            | "objective-c++-header"
            | "objective-c++-cpp-output" => Some(Language::Cpp),
            _ => None,
        }
    }
}

enum DriverArgument<'a> {
    Operand(Operand, &'a str),
    Sysroot(&'a str),
    Language(&'a str),
    Input(&'a str),
    Flag(&'a str),
}

impl<'a> DriverArgument<'a> {
    fn visit(
        args: &'a [std::borrow::Cow<'_, str>],
        mut observe: impl FnMut(Self) -> Option<()>,
    ) -> Option<()> {
        let mut args = args.iter().skip(1).map(AsRef::as_ref);
        while let Some(arg) = args.next() {
            if arg == "--" {
                return args.try_for_each(|path| observe(Self::Input(path)));
            }
            observe(Self::read(arg, &mut args)?)?;
        }
        Some(())
    }

    fn read(arg: &'a str, args: &mut impl Iterator<Item = &'a str>) -> Option<Self> {
        const DRIVER_VALUE_FLAGS: &[&str] = &[
            "-o",
            "-MF",
            "-MT",
            "-MQ",
            "-MJ",
            "-Xassembler",
            "-Xlinker",
            "-Xanalyzer",
            "-serialize-diagnostics",
            "-dependency-file",
            "-arch",
            "-target",
            "--target",
            "--sysroot",
        ];
        if !arg.starts_with(['-', '@']) {
            return Some(Self::Input(arg));
        }
        if opaque_compiler_flag(arg) {
            return None;
        }
        if DRIVER_VALUE_FLAGS.contains(&arg) {
            return args.next().map(|_| Self::Flag(arg));
        }
        let value_argument = [
            ("-x", Self::Language as fn(&'a str) -> Self),
            ("-isysroot", Self::Sysroot),
        ];
        for (prefix, constructor) in value_argument {
            if let Some(value) = arg.strip_prefix(prefix) {
                return argument_value(value, args).map(constructor);
            }
        }
        let Some((kind, value)) = Operand::split(arg) else {
            return Some(Self::Flag(arg));
        };
        argument_value(value, args).map(|value| Self::Operand(kind, value))
    }
}

fn argument_value<'a>(
    attached: &'a str,
    args: &mut impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    if attached.is_empty() {
        args.next()
    } else {
        Some(attached)
    }
}

struct InputDialect<'a> {
    mode: &'a str,
    selected: Option<Language>,
}

impl<'a> InputDialect<'a> {
    fn observe(&mut self, argument: DriverArgument<'a>, source: &DriverSource) -> Option<()> {
        match argument {
            DriverArgument::Language(mode) => self.mode = mode,
            DriverArgument::Input(value) if source.matches(value) => {
                source.record_language(self.mode, &mut self.selected)?;
            }
            _ => {}
        }
        Some(())
    }
}

impl<'a> DriverOptions<'a> {
    // First collect effective operands. Header search policy can depend on a
    // sysroot appearing later, and user defines must follow compiler defaults.
    fn read(
        args: &'a [std::borrow::Cow<'_, str>],
        source: &DriverSource,
    ) -> Option<(Self, Language)> {
        let mut options = Self::default();
        let mut dialect = InputDialect {
            mode: "none",
            selected: None,
        };
        let rebase = |value: &str| source.dir.operand(source.root, value);
        DriverArgument::visit(args, |arg| {
            match arg {
                DriverArgument::Operand(kind, value) => {
                    options
                        .operands
                        .push((kind, kind.spelling(value, &rebase)?));
                }
                DriverArgument::Sysroot(path) => {
                    options
                        .sysroots
                        .push(rebase(path).unwrap_or_else(|| PathBuf::from(path)));
                }
                DriverArgument::Flag(flag) => options.observe_driver_flag(flag),
                input => dialect.observe(input, source)?,
            }
            Some(())
        })?;
        Some((options, dialect.selected.unwrap_or(source.default)))
    }

    fn observe_driver_flag(&mut self, arg: &'a str) {
        match arg {
            "-funsigned-char" => self.unsigned_char = Some(true),
            "-fsigned-char" => self.unsigned_char = Some(false),
            _ => {}
        }
        if let Some(standard) = arg.strip_prefix("-std=") {
            self.standard = Some(standard);
        }
        self.replaced_headers |= arg == "-ffreestanding"
            || ["-nostdinc", "--sysroot", "-isysroot"]
                .iter()
                .any(|p| arg.starts_with(p));
        self.target_override |= matches!(
            arg,
            "-undef" | "-fms-compatibility" | "-arch" | "-EB" | "-EL"
        ) || ["-m", "-fshort-", "--target", "-target"]
            .iter()
            .any(|p| arg.starts_with(p));
    }
    fn native_headers(&self) -> bool {
        !self.replaced_headers && self.sysroots.iter().all(|root| super::sdk::trusted(root))
    }
    fn system_path(&self, path: &str) -> bool {
        let p = Path::new(path);
        self.sysroots.iter().any(|root| p.starts_with(root))
            || p.starts_with("/usr/include")
            || path.contains("/lib/clang/")
            || path.ends_with(".xctoolchain/usr/include")
    }
    fn native_framework(&self, path: &str) -> bool {
        self.sysroots.iter().any(|root| {
            Path::new(path).strip_prefix(root).ok().is_some_and(|rel| {
                rel == Path::new("System/Library/Frameworks")
                    || rel == Path::new("System/Library/SubFrameworks")
            })
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operand {
    Framework,
    Define,
    Undefine,
    Directory(Directory),
    ForcedHeader,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Directory {
    Include,
    Quote,
    System,
}

impl Operand {
    fn split(arg: &str) -> Option<(Self, &str)> {
        [
            ("-F", Self::Framework),
            ("-D", Self::Define),
            ("-U", Self::Undefine),
            ("-I", Self::Directory(Directory::Include)),
            ("-iquote", Self::Directory(Directory::Quote)),
            ("-isystem", Self::Directory(Directory::System)),
            ("-include", Self::ForcedHeader),
        ]
        .into_iter()
        .find_map(|(prefix, kind)| Some((kind, arg.strip_prefix(prefix)?)))
    }

    fn spelling<'a>(
        self,
        value: &'a str,
        rebase: &impl Fn(&str) -> Option<PathBuf>,
    ) -> Option<std::borrow::Cow<'a, str>> {
        if matches!(
            self,
            Self::Directory(_) | Self::ForcedHeader | Self::Framework
        ) {
            if let Some(path) = rebase(value) {
                return Some(std::borrow::Cow::Owned(
                    path.into_os_string().into_string().ok()?,
                ));
            }
        }
        Some(std::borrow::Cow::Borrowed(value))
    }
}

// An SDK search path may legitimately contain application-specific headers.
// Only model the host's integer limits when standard headers can come solely
// from its native include roots. In-repository shadows are parsed directly.
fn native_integer_layout(path: &Path) -> bool {
    if path == Path::new("/usr/include")
        || path.ends_with("XcodeDefault.xctoolchain/usr/include")
        || path.ends_with("CommandLineTools/usr/include")
    {
        return true;
    }
    let sdk_include = if path.ends_with("c++/v1") {
        path.parent().and_then(Path::parent).unwrap_or(path)
    } else {
        path
    };
    if super::sdk::sdk_include_root(sdk_include).is_some() {
        return true;
    }
    path.file_name().is_some_and(|name| name == "include")
        && path
            .parent()
            .and_then(Path::parent)
            .is_some_and(|p| p.ends_with("lib/clang"))
}

fn opaque_compiler_flag(arg: &str) -> bool {
    arg.starts_with('@')
        || matches!(arg, "-Wp" | "-Xpreprocessor")
        || arg.starts_with("-Wp,")
        || arg.starts_with("-include-pch")
        || [
            "-imacros",
            "-Xclang",
            "-idirafter",
            "-iframework",
            "-iwithprefix",
            "-iprefix",
            "-ivfsoverlay",
            "-resource-dir",
            "-fmodules",
            "-internal-isystem",
            "-internal-externc-isystem",
            "-working-directory",
            "-I-",
            "-isystem-after",
        ]
        .iter()
        .any(|prefix| arg.starts_with(prefix))
}

fn language_version(std: &str, language: Language) -> Option<i64> {
    if language == Language::C {
        return matches!(std, "c23" | "c2x" | "gnu23" | "gnu2x" | "iso9899:2024").then_some(202311);
    }
    cpp_standard(std)
}
fn cpp_standard(std: &str) -> Option<i64> {
    const VERSIONS: &[(&[&str], i64)] = &[
        (&["98", "03"], 199711),
        (&["11", "0x"], 201103),
        (&["14", "1y"], 201402),
        (&["17", "1z"], 201703),
        (&["20", "2a"], 202002),
        (&["23"], 202302),
    ];
    let suffix = std
        .strip_prefix("gnu++")
        .or_else(|| std.strip_prefix("c++"))?;
    VERSIONS
        .iter()
        .find_map(|(aliases, version)| aliases.contains(&suffix).then_some(*version))
}

#[cfg(test)]
#[test]
fn dialect_spelling_selects_the_effective_version() {
    for (suffix, version) in [
        ("98", 199711),
        ("03", 199711),
        ("11", 201103),
        ("0x", 201103),
        ("14", 201402),
        ("1y", 201402),
        ("17", 201703),
        ("1z", 201703),
        ("20", 202002),
        ("2a", 202002),
        ("23", 202302),
    ] {
        for prefix in ["c++", "gnu++"] {
            let spelling = format!("{prefix}{suffix}");
            assert_eq!(language_version(&spelling, Language::Cpp), Some(version));
            assert_eq!(language_version(&spelling, Language::C), None);
        }
    }
    for spelling in ["gnu++2b", "c++26", "gnu23", "c23", "23"] {
        assert_eq!(language_version(spelling, Language::Cpp), None);
    }
    assert_eq!(language_version("gnu23", Language::C), Some(202311));
}

// Effective search roots and deferred -isystem operands belong to one command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Location {
    Captured,
    Native,
    Opaque,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchRoot {
    path: String,
    location: Location,
}

#[derive(Default, Clone, Debug, PartialEq, Eq)]
struct SearchDirs {
    quote: Vec<SearchRoot>,
    includes: Vec<SearchRoot>,
    // SDK reads use a stable ordered slice as their cache key.
    sdk: Vec<String>,
}

impl SearchDirs {
    fn all(&self) -> impl Iterator<Item = &SearchRoot> {
        self.quote.iter().chain(&self.includes)
    }
    fn lookup<'a>(
        &'a self,
        quoted_from: Option<&'a Path>,
    ) -> impl Iterator<Item = (&'a Path, Location)> + 'a {
        let parent = quoted_from.and_then(Path::parent).map(|path| {
            let location = if path.is_absolute() {
                self.all()
                    .find(|entry| path == Path::new(&entry.path))
                    .map_or(Location::Native, |r| r.location)
            } else {
                Location::Captured
            };
            (path, location)
        });
        parent.into_iter().chain(
            quoted_from
                .into_iter()
                .flat_map(|_| &self.quote)
                .chain(&self.includes)
                .map(|dir| (Path::new(&dir.path), dir.location)),
        )
    }
}

struct HeaderSearch<'a> {
    root: &'a Path,
    dir: &'a Path,
    options: DriverOptions<'a>,
    system_dirs: Vec<SearchRoot>,
}

impl HeaderSearch<'_> {
    fn add(&mut self, command: &mut Command, kind: Directory, value: &str) {
        let path = inside(self.root, self.dir, value)
            .unwrap_or_else(|| self.dir.join(value).to_string_lossy().into_owned());
        let location = self.location(&path, kind, &mut command.native);
        let list = match kind {
            Directory::Quote => &mut command.search.quote,
            Directory::Include => &mut command.search.includes,
            Directory::System => &mut self.system_dirs,
        };
        list.push(SearchRoot { path, location });
    }
    fn location(&self, path: &str, kind: Directory, native_integers: &mut bool) -> Location {
        let directory = Path::new(path);
        if !directory.is_absolute() {
            return Location::Captured;
        }
        if kind != Directory::System {
            return Location::Opaque;
        }
        let trusted = super::sdk::trusted(directory);
        *native_integers &= trusted && native_integer_layout(directory);
        if trusted && self.options.system_path(path) {
            Location::Native
        } else {
            Location::Opaque
        }
    }
    fn finish(self, command: &mut Command) {
        let search = &mut command.search;
        let captured: std::collections::HashSet<_> = self
            .system_dirs
            .iter()
            .filter(|dir| dir.location == Location::Captured)
            .map(|dir| &dir.path)
            .collect();
        search.includes.retain(|dir| !captured.contains(&dir.path));
        search.includes.extend(self.system_dirs);
        // An opaque spelling of a path overrides any later trusted spelling.
        let opaque: std::collections::HashSet<_> = search
            .all()
            .filter(|dir| dir.location == Location::Opaque)
            .map(|dir| dir.path.clone())
            .collect();
        for root in search.quote.iter_mut().chain(&mut search.includes) {
            if root.location == Location::Native && opaque.contains(&root.path) {
                root.location = Location::Opaque;
            }
        }
        search.sdk = search
            .includes
            .iter()
            .filter(|dir| dir.location == Location::Native)
            .map(|dir| dir.path.clone())
            .collect();
    }
}

impl Command {
    pub(super) fn mode(&self) -> lexer::Mode {
        let mut mode = lexer::Mode::written(self.language());
        if self.standard.is_some() {
            mode.digit_separators = self.digit_separators();
        }
        mode
    }
    pub(super) fn digit_separators(&self) -> bool {
        self.standard
            .is_some_and(|version| version >= if self.cpp { 201402 } else { 202311 })
    }
    pub(super) fn language(&self) -> Language {
        if self.cpp { Language::Cpp } else { Language::C }
    }
    fn parse(
        root: &Path,
        dir: &database::Directory,
        file: &str,
        args: &[std::borrow::Cow<'_, str>],
    ) -> Option<Self> {
        let driver = args
            .first()
            .and_then(|arg| Path::new(arg.as_ref()).file_name())
            .and_then(|d| d.to_str());
        let cpp = matches!(Language::for_path(file), Some(Language::Cpp))
            || driver.is_some_and(|name| name.ends_with("++"));
        let source = DriverSource {
            root,
            dir,
            file,
            default: if cpp { Language::Cpp } else { Language::C },
        };
        let (mut options, language) = DriverOptions::read(args, &source)?;
        let operands = std::mem::take(&mut options.operands);
        let mut command = Self {
            cpp: language == Language::Cpp,
            defines: HashMap::new(),
            search: SearchDirs::default(),
            forced: Vec::new(),
            standard: options.standard.and_then(|s| language_version(s, language)),
            native: false,
            unsigned_char: options.unsigned_char,
        };
        let mut search = HeaderSearch {
            root,
            dir: &dir.path,
            options,
            system_dirs: Vec::new(),
        };
        command.compiler_defaults(driver, &search.options);
        for (operand, value) in operands {
            command.apply_operand(operand, &value, &mut search)?;
        }
        search.finish(&mut command);
        Some(command)
    }
    fn apply_operand(
        &mut self,
        flag: Operand,
        value: &str,
        search: &mut HeaderSearch<'_>,
    ) -> Option<()> {
        match flag {
            Operand::Framework => return search.options.native_framework(value).then_some(()),
            Operand::Define => {
                let (key, value) = value.split_once('=').unwrap_or((value, "1"));
                self.defines.insert(key.to_owned(), Some(value.to_owned()));
            }
            Operand::Undefine => {
                self.defines.insert(value.to_owned(), None);
            }
            Operand::ForcedHeader => {
                self.forced.push((
                    inside(search.root, search.dir, value).unwrap_or_else(|| value.to_owned()),
                    value.to_owned(),
                ));
            }
            Operand::Directory(kind) => search.add(self, kind, value),
        }
        Some(())
    }
    fn compiler_defaults(&mut self, driver: Option<&str>, options: &DriverOptions) {
        let native_driver = matches!(
            driver,
            Some("cc" | "c++" | "gcc" | "g++" | "clang" | "clang++")
        );
        if !native_driver || options.target_override {
            return;
        }
        // The unprefixed native GNU/Clang drivers default to the host target.
        // Explicit cross targets retain only arguments supplied by the database.
        self.native = options.native_headers();
        for (name, value) in [
            ("__MSDOS__", None),
            ("__linux__", cfg!(target_os = "linux").then_some("1")),
            ("__APPLE__", cfg!(target_vendor = "apple").then_some("1")),
            ("__CHAR_BIT__", Some("8")),
            ("__ORDER_LITTLE_ENDIAN__", Some("1234")),
            ("__ORDER_BIG_ENDIAN__", Some("4321")),
            (
                "__BYTE_ORDER__",
                Some(if cfg!(target_endian = "little") {
                    "1234"
                } else {
                    "4321"
                }),
            ),
        ] {
            self.defines.insert(name.into(), value.map(str::to_owned));
        }
        if !cfg!(target_os = "windows") {
            self.defines.insert("_WIN32".into(), None);
        }
        let mut define = |name: &str, value: &str| {
            self.defines.insert(name.into(), Some(value.into()));
        };
        if matches!(driver, Some("clang" | "clang++")) {
            define("__clang__", "1");
            define("__GNUC__", "4");
        } else if matches!(driver, Some("gcc" | "g++")) {
            define("__GNUC__", "");
        }
    }
}

pub(super) enum Include<'a> {
    Captured(&'a str, Arc<PreprocessingSource<'a>>),
    External(Option<Arc<Proof>>),
    Opaque,
}

impl<'a> Include<'a> {
    fn captured(self) -> Option<(&'a str, Arc<PreprocessingSource<'a>>)> {
        match self {
            Self::Captured(path, source) => Some((path, source)),
            _ => None,
        }
    }
}

pub(super) struct IncludeQuery<'a> {
    pub name: &'a str,
    pub quoted_from: Option<&'a Path>,
    pub optional: bool,
}

impl<'a> CProject<'a> {
    pub(crate) fn recognizes(path: &str) -> bool {
        Language::for_path(path).is_some_and(Self::supports)
    }
    pub(crate) fn supports(language: Language) -> bool {
        matches!(language, Language::C | Language::Cpp)
    }
    pub(crate) fn new(
        root: &Path,
        databases: impl Iterator<Item = (&'a str, &'a str)>,
        lookup: impl Fn(&str) -> Option<(&'a str, &'a str)> + Sync + 'a,
    ) -> Self {
        let mut project = Self {
            commands: HashMap::new(),
            consulted: Mutex::default(),
            lookup: Box::new(lookup),
            headers: Mutex::default(),
            sdk: Sdk::default(),
        };
        for (path, text) in databases {
            project.database(root, path, text);
        }
        project
    }
    pub(crate) fn consulted(self) -> (HashMap<String, Vec<String>>, Vec<Receipt>) {
        (self.consulted.into_inner().unwrap(), self.sdk.receipts())
    }
    /// Compare the effective settings for each translation unit independently of
    /// database order or duplicate entries. An unreadable command still matters.
    pub(crate) fn command_keys(&self) -> HashMap<String, Vec<String>> {
        self.commands
            .iter()
            .map(|(path, commands)| {
                let mut keys: Vec<_> = commands
                    .iter()
                    .map(|c| {
                        c.as_ref()
                            .map_or_else(|| "unknown".into(), |c| c.context_key(path))
                    })
                    .collect();
                keys.sort_unstable();
                (path.clone(), keys)
            })
            .collect()
    }
    pub(crate) fn put_dependencies(
        analysis: &mut Analysis,
        deps: HashMap<String, Vec<String>>,
        receipts: Vec<Receipt>,
        keys: HashMap<String, Vec<String>>,
    ) {
        analysis.direct_evidence = Some(Box::new(super::super::direct::Evidence::CIncludes(
            CompileInputs {
                paths: deps,
                receipts,
                commands: Some(keys),
            },
        )));
    }
    pub(crate) fn take_dependencies(analysis: Analysis) -> Option<CompileInputs> {
        match *analysis.direct_evidence? {
            super::super::direct::Evidence::CIncludes(inputs) => Some(inputs),
            _ => None,
        }
    }
    fn sdk_proof(
        &self,
        name: &str,
        cmd: &Command,
        optional: bool,
        consulted: &mut std::collections::HashSet<String>,
    ) -> Option<Arc<Proof>> {
        if cmd.search.sdk.is_empty() {
            return None;
        }
        let proof = self.sdk.proof(&cmd.search.sdk, name, optional, consulted)?;
        let mut shadow = false;
        for dir in cmd
            .search
            .all()
            .filter(|dir| dir.location == Location::Captured)
            .map(|dir| &dir.path)
        {
            for header in proof.nested_headers() {
                let path = normalize(&Path::new(dir).join(header))?;
                let path = path.to_str()?;
                consulted.insert(path.to_owned());
                shadow |= (self.lookup)(path).is_some();
            }
        }
        (!shadow).then_some(proof)
    }
    fn captured_header(
        &self,
        source: (&'a str, &'a str),
        mode: lexer::Mode,
    ) -> (&'a str, Arc<PreprocessingSource<'a>>) {
        let (path, text) = source;
        let slot = Arc::clone(
            self.headers
                .lock()
                .unwrap()
                .entry((path, mode))
                .or_default(),
        );
        (
            path,
            Arc::clone(slot.get_or_init(|| Arc::new(PreprocessingSource::new(text, mode)))),
        )
    }

    pub(super) fn header(
        &self,
        path: &str,
        mode: lexer::Mode,
    ) -> Option<(&'a str, Arc<PreprocessingSource<'a>>)> {
        Some(self.captured_header((self.lookup)(path)?, mode))
    }

    fn captured_at(
        &self,
        dir: &Path,
        name: &str,
        consulted: &mut std::collections::HashSet<String>,
    ) -> Option<(&'a str, &'a str)> {
        let path = normalize(&dir.join(name))?;
        let spelling = path.to_str()?;
        consulted.insert(spelling.to_owned());
        (self.lookup)(spelling)
    }

    pub(super) fn include(
        &self,
        query: IncludeQuery<'_>,
        command: &Command,
        consulted: &mut std::collections::HashSet<String>,
    ) -> Include<'a> {
        let search = self.find_include(query.name, query.quoted_from, command, consulted);
        match search {
            Include::External(_) => {
                Include::External(self.sdk_proof(query.name, command, query.optional, consulted))
            }
            _ => search,
        }
    }

    pub(super) fn forced_header(
        &self,
        candidate: &str,
        spelling: &str,
        command: &Command,
        consulted: &mut std::collections::HashSet<String>,
    ) -> Option<(&'a str, Arc<PreprocessingSource<'a>>)> {
        consulted.insert(candidate.to_owned());
        self.header(candidate, command.mode()).or_else(|| {
            self.find_include(spelling, Some(Path::new("")), command, consulted)
                .captured()
        })
    }

    fn find_include(
        &self,
        name: &str,
        quoted_from: Option<&Path>,
        command: &Command,
        consulted: &mut std::collections::HashSet<String>,
    ) -> Include<'a> {
        let mut search = IncludeSearch {
            name,
            absolute: Path::new(name).is_absolute(),
            external: false,
            opaque: false,
        };
        let found = command
            .search
            .lookup(quoted_from)
            .find_map(|(dir, location)| search.observe(self, dir, location, consulted));
        match found {
            Some(LocatedInclude::Captured(source)) => {
                let (path, source) = self.captured_header(source, command.mode());
                Include::Captured(path, source)
            }
            Some(LocatedInclude::Shadowed) => Include::Opaque,
            None if search.opaque => Include::Opaque,
            None => Include::External(None),
        }
    }
}

enum LocatedInclude<'a> {
    Captured((&'a str, &'a str)),
    Shadowed,
}

struct IncludeSearch<'a> {
    name: &'a str,
    absolute: bool,
    external: bool,
    opaque: bool,
}

impl IncludeSearch<'_> {
    fn observe<'src>(
        &mut self,
        project: &CProject<'src>,
        dir: &Path,
        location: Location,
        consulted: &mut std::collections::HashSet<String>,
    ) -> Option<LocatedInclude<'src>> {
        if location != Location::Captured || self.absolute {
            self.external = true;
            self.opaque |= location == Location::Opaque;
            return None;
        }
        let source = project.captured_at(dir, self.name, consulted)?;
        // A shadowed candidate still contributes a dependency observation.
        Some(if self.external {
            LocatedInclude::Shadowed
        } else {
            LocatedInclude::Captured(source)
        })
    }
}

/// Captured preprocessing inputs selected before entering the token loop.
pub(crate) struct TranslationUnit<'p, 'src> {
    pub(super) written: Language,
    project: &'p CProject<'src>,
    commands: UnitCommands<'p>,
}

#[derive(Clone, Copy)]
enum UnitCommands<'p> {
    Configured(&'p Command),
    Written(&'p [Option<Command>]),
}
impl UnitCommands<'_> {
    fn mode(self, written: Language) -> lexer::Mode {
        match self {
            Self::Configured(cmd) => cmd.mode(),
            Self::Written(_) => lexer::Mode::written(written),
        }
    }
}

impl<'src> super::super::ProjectInputs for CProject<'src> {
    type Context<'p>
        = TranslationUnit<'p, 'src>
    where
        Self: 'p;

    fn prepare(&self, written: Language, path: &str) -> Option<Self::Context<'_>> {
        if !Self::supports(written) {
            return None;
        }
        let commands = self.commands.get(path).map_or(&[][..], Vec::as_slice);
        let commands = match commands {
            [Some(command)] => UnitCommands::Configured(command),
            others => UnitCommands::Written(others),
        };
        Some(TranslationUnit {
            written,
            project: self,
            commands,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) enum RowReuse {
    Stream,
    Shared,
}

impl RowReuse {
    fn for_source(shared: bool) -> Self {
        if shared { Self::Shared } else { Self::Stream }
    }
}

#[derive(Clone, Copy)]
pub(super) struct TranslationInput<'a> {
    pub(super) path: &'a str,
    pub(super) source: &'a PreprocessingSource<'a>,
    pub(super) reuse: RowReuse,
}

struct PartitionSources<'a> {
    written: &'a PreprocessingSource<'a>,
    counts: HashMap<lexer::Mode, usize>,
    alternate: HashMap<lexer::Mode, PreprocessingSource<'a>>,
}

impl<'a> PartitionSources<'a> {
    fn input<'b>(&'b mut self, path: &'b str, mode: lexer::Mode) -> TranslationInput<'b> {
        if mode == self.written.mode {
            return TranslationInput {
                path,
                source: self.written,
                reuse: RowReuse::Shared,
            };
        }
        let source = self
            .alternate
            .entry(mode)
            .or_insert_with(|| PreprocessingSource::new(self.written.text, mode));
        TranslationInput {
            path,
            source,
            reuse: RowReuse::for_source(self.counts[&mode] > 1),
        }
    }
}

#[derive(Default)]
struct TranslationEvidence {
    paths: Vec<String>,
    partitions: Vec<super::translation::Partition>,
}
impl TranslationEvidence {
    fn include(
        &mut self,
        (paths, partition): (
            std::collections::HashSet<String>,
            Option<super::translation::Partition>,
        ),
    ) {
        self.paths.extend(paths);
        self.partitions.extend(partition);
    }
    fn publish(mut self, project: &CProject, path: &str, facts: &mut Analysis) {
        self.paths.sort_unstable();
        self.paths.dedup();
        project
            .consulted
            .lock()
            .unwrap()
            .insert(path.to_owned(), self.paths);
        if !self.partitions.is_empty() {
            self.partitions.sort_by(|a, b| a.key.cmp(&b.key));
            facts.direct_evidence = Some(Box::new(super::super::direct::Evidence::Translation(
                self.partitions,
            )));
        }
    }
}
impl TranslationUnit<'_, '_> {
    fn partitions(
        &self,
        commands: &[Option<Command>],
        path: &str,
        written: &PreprocessingSource<'_>,
    ) -> TranslationEvidence {
        let requests: Vec<_> = commands
            .iter()
            .flatten()
            .map(|command| (command, command.mode()))
            .collect();
        let mut counts = HashMap::new();
        for &(_, mode) in &requests {
            *counts.entry(mode).or_insert(0usize) += 1;
        }

        let mut sources = PartitionSources {
            written,
            counts,
            alternate: HashMap::new(),
        };
        let mut result = TranslationEvidence::default();
        for (command, mode) in requests {
            result.include(super::directives::configured_partition(
                self.project,
                command,
                sources.input(path, mode),
            ));
        }
        result
    }
    fn preprocess(
        &self,
        input: TranslationInput,
        graph: bool,
        analyzer: &mut super::Analyzer<'_, '_>,
    ) -> Option<TranslationEvidence> {
        match self.commands {
            UnitCommands::Configured(command) => {
                analyzer.digit_separators = command.digit_separators();
                let products = analyzer.project_directives(self.project, command, input, graph);
                graph.then(|| {
                    let mut result = TranslationEvidence::default();
                    result.include(products);
                    result
                })
            }
            UnitCommands::Written(commands) => {
                let result = (graph && !commands.is_empty())
                    .then(|| self.partitions(commands, input.path, input.source));
                // A compatible partition may have left its projected rows for this written pass.
                analyzer.source_directives(input);
                result
            }
        }
    }

    pub(in crate::domain::code) fn extract<T>(
        &self,
        path: &str,
        text: &str,
        graph: bool,
        consume: impl FnOnce(Language, &[Token], Analysis, &[usize]) -> T,
    ) -> T {
        let project = self.project;
        let mode = self.commands.mode(self.written);
        let lang = mode.language;
        // Header analysis and repeated inclusions share the immutable token buffer.
        let header = matches!(
            Path::new(path).extension().and_then(|s| s.to_str()),
            Some("h" | "hh" | "hpp" | "hxx")
        )
        .then(|| project.header(path, mode))
        .flatten();
        let local;
        let written = if let Some((_, source)) = &header {
            source.as_ref()
        } else {
            local = PreprocessingSource::new(text, mode);
            &local
        };
        let tokens = written.tokens.as_slice();
        let mut analyzer = super::Analyzer::new(lang, tokens);
        let input = TranslationInput {
            path,
            source: written,
            reuse: RowReuse::for_source(header.is_some()),
        };
        let evidence = self.preprocess(input, graph, &mut analyzer);
        analyzer.extract_names();
        if let Some(evidence) = evidence {
            evidence.publish(project, path, &mut analyzer.out);
        }
        consume(lang, tokens, analyzer.out, &analyzer.pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysroot_path_has_the_same_meaning_attached_or_separate() {
        let root = Path::new("/repo");
        let dir = database::Directory::new(root, root, ".");
        let parse = |args: &[&str]| {
            let args = args
                .iter()
                .copied()
                .map(std::borrow::Cow::Borrowed)
                .collect::<Vec<_>>();
            let source = DriverSource {
                root,
                dir: &dir,
                file: "main.c",
                default: Language::C,
            };
            DriverOptions::read(&args, &source).unwrap().0.sysroots
        };
        assert_eq!(
            parse(&["clang", "-isysroot", "/sdk"]),
            [PathBuf::from("/sdk")]
        );
        assert_eq!(parse(&["clang", "-isysroot/sdk"]), [PathBuf::from("/sdk")]);
    }

    #[test]
    fn compiler_language_is_bound_to_the_input_operand() {
        let root = Path::new("/repo");
        let dir = database::Directory::new(root, root, ".");
        let dialect = |args: &[&str]| {
            let args = args
                .iter()
                .copied()
                .map(std::borrow::Cow::Borrowed)
                .collect::<Vec<_>>();
            Command::parse(root, &dir, "main.c", &args).map(|command| command.cpp)
        };
        assert_eq!(
            dialect(&["clang", "-xc++", "-I", "main.c", "-xc", "main.c"]),
            Some(false)
        );
        assert_eq!(
            dialect(&["clang", "-xc++", "main.c", "-xc", "other.c"]),
            Some(true)
        );
        assert_eq!(
            dialect(&["clang", "-xc++", "main.c", "-xc", "main.c"]),
            None
        );
    }

    #[test]
    fn flag_shaped_include_operand_is_not_a_driver_switch() {
        let root = Path::new("/repo");
        let parse = |args: &[&str]| {
            let args = args
                .iter()
                .copied()
                .map(std::borrow::Cow::Borrowed)
                .collect::<Vec<_>>();
            Command::parse(
                root,
                &database::Directory::new(root, root, "."),
                "main.c",
                &args,
            )
            .unwrap()
        };
        let ordinary = parse(&["clang", "-I", "inc"]);
        let operand = parse(&["clang", "-I", "-target-placeholder"]);
        assert_eq!(operand.native, ordinary.native);
        assert_eq!(operand.defines, ordinary.defines);
        assert_eq!(operand.search.includes[0].path, "-target-placeholder");
        for flag in ["-serialize-diagnostics", "-Xanalyzer", "-dependency-file"] {
            let command = parse(&["clang", flag, "-DIGNORED=1", "-DREAL=2"]);
            assert!(!command.defines.contains_key("IGNORED"));
            assert_eq!(command.defines["REAL"].as_deref(), Some("2"));
        }
        assert!(!parse(&["clang", "-target", "unknown"]).native);
    }

    #[test]
    fn shadowed_repository_headers_are_observed_without_tokenizing() {
        let root = Path::new("/repo");
        let project = CProject::new(root, std::iter::empty(), |path| {
            (path == "include/f.h").then_some(("include/f.h", "#define F 1"))
        });
        let mut command = Command::parse(
            root,
            &database::Directory::new(root, root, "."),
            "main.c",
            &[],
        )
        .unwrap();
        command.search.includes = vec![
            SearchRoot {
                path: "/outside".into(),
                location: Location::Opaque,
            },
            SearchRoot {
                path: "include".into(),
                location: Location::Captured,
            },
        ];
        let mut consulted = std::collections::HashSet::new();
        assert!(matches!(
            project.find_include("f.h", None, &command, &mut consulted),
            Include::Opaque
        ));
        assert!(consulted.contains("include/f.h"));
        assert!(project.headers.lock().unwrap().is_empty());
        command.search.includes.remove(0);
        assert!(
            project
                .find_include("f.h", None, &command, &mut consulted)
                .captured()
                .is_some()
        );
        assert_eq!(project.headers.lock().unwrap().len(), 1);
    }

    #[test]
    fn repeated_includes_share_rows_only_within_the_same_dialect() {
        let text = "int one;\nint two;\n#if 1 && \\\n  1\nint three;\n#endif\n";
        let project = CProject::new(Path::new("."), std::iter::empty(), |_| Some(("a.h", text)));
        let (_, first) = project
            .header("a.h", lexer::Mode::written(Language::C))
            .unwrap();
        let (_, second) = project
            .header("a.h", lexer::Mode::written(Language::C))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        // The ordinary declarations group together; the continued guard is one row.
        assert_eq!(first.rows().len(), 4);
        assert!(std::ptr::eq(first.rows(), second.rows()));
        let (_, other) = project
            .header("a.h", lexer::Mode::written(Language::Cpp))
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &other));
    }
}
