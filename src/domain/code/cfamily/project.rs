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
    pub includes: Vec<String>,
    pub quote_dirs: Vec<String>,
    pub forced: Vec<(String, String)>,
    pub standard: Option<i64>,
    pub native: bool,
    pub unsigned_char: Option<bool>,
    pub opaque_dirs: std::collections::HashSet<String>,
    pub defines: HashMap<String, Option<String>>,
    pub cpp: bool,
}
type Lookup<'a> = dyn Fn(&str) -> Option<(&'a str, &'a str)> + Sync + 'a;
type TokenCache<'a> = Mutex<HashMap<(&'a str, bool), Arc<OnceLock<Arc<[Token<'a>]>>>>>;
pub(crate) struct CProject<'a> {
    commands: HashMap<String, Option<Command>>,
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

fn json_text(t: &Token) -> Option<String> {
    let raw = t.text.strip_prefix('"')?.strip_suffix('"')?;
    let mut chars = raw.chars();
    let mut value = String::new();
    while let Some(c) = chars.next() {
        value.push(if c == '\\' {
            match chars.next()? {
                c @ ('"' | '/' | '\\') => c,
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'b' => '\x08',
                'f' => '\x0c',
                'u' => {
                    let code: String = chars.by_ref().take(4).collect();
                    if code.len() != 4 {
                        return None;
                    }
                    char::from_u32(u32::from_str_radix(&code, 16).ok()?)?
                }
                _ => return None,
            }
        } else {
            c
        });
    }
    Some(value)
}

// Compilation databases may store a POSIX-quoted command in place of argv.
// Reject substitutions and shell operators. No shell is ever invoked.
fn split_command(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            c if quote == Some(c) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(c),
            '\\' if quote != Some('\'') => word.push(chars.next()?),
            '$' | '`' if quote != Some('\'') => return None,
            ';' | '|' | '&' | '>' | '<' if quote.is_none() => return None,
            c if c.is_whitespace() && quote.is_none() => {
                if started {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
                continue;
            }
            _ => word.push(c),
        }
        started = true;
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(word);
    }
    Some(words)
}

// SDK directories supplied by a compile database can describe the native
// system header search. User-supplied, unobserved directories stay opaque.
fn system_path(path: &str, args: &[String]) -> bool {
    let p = Path::new(path);
    super::sdk::trusted(p)
        && (args
            .windows(2)
            .any(|v| v[0] == "-isysroot" && p.starts_with(&v[1]))
            || p.starts_with("/usr/include")
            || path.contains("/lib/clang/")
            || path.ends_with(".xctoolchain/usr/include"))
}

// An SDK search path may legitimately contain application-specific headers.
// Only model the host's integer limits when standard headers can come solely
// from its native include roots. In-repository shadows are parsed directly.
fn native_integer_root(path: &Path) -> bool {
    if !super::sdk::trusted(path) {
        return false;
    }
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
    if sdk_include.ends_with("usr/include")
        && sdk_include
            .parent()
            .and_then(Path::parent)
            .and_then(Path::extension)
            .is_some_and(|e| e == "sdk")
    {
        return true;
    }
    path.file_name().is_some_and(|name| name == "include")
        && path
            .parent()
            .and_then(Path::parent)
            .is_some_and(|p| p.ends_with("lib/clang"))
}

impl Command {
    fn parse(root: &Path, dir: &Path, file: &str, args: &[String]) -> Option<Self> {
        let mut command = Self {
            cpp: matches!(Language::for_path(file), Some(Language::Cpp)),
            defines: HashMap::new(),
            includes: Vec::new(),
            quote_dirs: Vec::new(),
            forced: Vec::new(),
            standard: None,
            native: false,
            unsigned_char: None,
            opaque_dirs: Default::default(),
        };
        command.compiler_defaults(args);
        // Header values correspond to the native hosted SDK. A replacement
        // sysroot or a freestanding header search has no such guarantee.
        if args.iter().any(|arg| {
            arg.starts_with("-nostdinc")
                || arg == "-ffreestanding"
                || arg.starts_with("--sysroot")
                || (arg.starts_with("-isysroot") && arg != "-isysroot")
        }) || args
            .windows(2)
            .any(|pair| pair[0] == "-isysroot" && !super::sdk::trusted(Path::new(&pair[1])))
        {
            command.native = false;
        }
        let mut system_dirs = Vec::new();
        if args
            .first()
            .and_then(|arg| Path::new(arg).file_name())
            .is_some_and(|arg| arg.to_string_lossy().ends_with("++"))
        {
            command.cpp = true;
        }
        let mut it = args.iter().skip(1);
        while let Some(arg) = it.next() {
            if let Some(rest) = arg.strip_prefix("-F") {
                let path = if rest.is_empty() {
                    it.next()?.as_str()
                } else {
                    rest
                };
                let native_framework_dir = args.windows(2).any(|v| {
                    v[0] == "-isysroot"
                        && Path::new(path).strip_prefix(&v[1]).ok().is_some_and(|rel| {
                            rel == Path::new("System/Library/Frameworks")
                                || rel == Path::new("System/Library/SubFrameworks")
                        })
                });
                if !native_framework_dir {
                    return None;
                }
                continue;
            }
            if arg.starts_with('@')
                || matches!(arg.as_str(), "-Wp" | "-Xpreprocessor")
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
            {
                return None;
            }
            match arg.as_str() {
                "-funsigned-char" => command.unsigned_char = Some(true),
                "-fsigned-char" => command.unsigned_char = Some(false),
                _ => {}
            }
            if let Some(std) = arg.strip_prefix("-std=") {
                let std = std
                    .strip_prefix("gnu++")
                    .or_else(|| std.strip_prefix("c++"));
                command.standard = match std {
                    Some("98" | "03") => Some(199711),
                    Some("11" | "0x") => Some(201103),
                    Some("14" | "1y") => Some(201402),
                    Some("17" | "1z") => Some(201703),
                    Some("20" | "2a") => Some(202002),
                    Some("23") => Some(202302),
                    _ => None,
                };
            }
            for flag in ["-D", "-U", "-I", "-iquote", "-isystem", "-include", "-x"] {
                let Some(rest) = arg.strip_prefix(flag) else {
                    continue;
                };
                let value = if rest.is_empty() {
                    it.next()?.as_str()
                } else {
                    rest
                };
                match flag {
                    "-D" => {
                        let (key, value) = value.split_once('=').unwrap_or((value, "1"));
                        command
                            .defines
                            .insert(key.to_owned(), Some(value.to_owned()));
                    }
                    "-U" => {
                        command.defines.insert(value.to_owned(), None);
                    }
                    "-include" => {
                        command.forced.push((
                            inside(root, dir, value).unwrap_or_else(|| value.to_owned()),
                            value.to_owned(),
                        ));
                    }
                    "-x" => command.cpp = value.contains("++"),
                    _ => {
                        let path = inside(root, dir, value)
                            .unwrap_or_else(|| dir.join(value).to_string_lossy().into_owned());
                        if Path::new(&path).is_absolute()
                            && flag == "-isystem"
                            && !native_integer_root(Path::new(&path))
                        {
                            command.native = false;
                        }
                        if Path::new(&path).is_absolute()
                            && (flag != "-isystem" || !system_path(&path, args))
                        {
                            command.opaque_dirs.insert(path.clone());
                        }
                        match flag {
                            "-iquote" => command.quote_dirs.push(path),
                            "-isystem" => system_dirs.push(path),
                            _ => command.includes.push(path),
                        }
                    }
                }
                break;
            }
        }
        command.includes.extend(system_dirs);
        Some(command)
    }
    fn compiler_defaults(&mut self, args: &[String]) {
        let driver = args
            .first()
            .and_then(|s| Path::new(s).file_name())
            .and_then(|s| s.to_str());
        let native_driver = matches!(
            driver,
            Some("cc" | "c++" | "gcc" | "g++" | "clang" | "clang++")
        );
        if !native_driver
            || args.iter().any(|s| {
                s == "-undef"
                    || s.starts_with("-m")
                    || s.starts_with("-fshort-")
                    || s == "-fms-compatibility"
                    || s.starts_with("--target")
                    || s.starts_with("-target")
                    || matches!(
                        s.as_str(),
                        "-arch" | "-mbig-endian" | "-mlittle-endian" | "-EB" | "-EL"
                    )
            })
        {
            return;
        }
        // The unprefixed native GNU/Clang drivers default to the host target.
        // Explicit cross targets retain only arguments supplied by the database.
        self.native = true;
        let mut define = |name: &str, value: &str| {
            self.defines.insert(name.into(), Some(value.into()));
        };
        if cfg!(target_vendor = "apple") {
            define("__APPLE__", "1");
        }
        if cfg!(target_os = "linux") {
            define("__linux__", "1");
        }
        define("__CHAR_BIT__", "8");
        define("__ORDER_LITTLE_ENDIAN__", "1234");
        define("__ORDER_BIG_ENDIAN__", "4321");
        define(
            "__BYTE_ORDER__",
            if cfg!(target_endian = "little") {
                "1234"
            } else {
                "4321"
            },
        );
        for (symbol, active) in [
            ("__MSDOS__", false),
            ("_WIN32", cfg!(target_os = "windows")),
            ("__linux__", cfg!(target_os = "linux")),
            ("__APPLE__", cfg!(target_vendor = "apple")),
        ] {
            if !active {
                self.defines.insert(symbol.into(), None);
            }
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

impl<'a> CProject<'a> {
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
    fn database(&mut self, root: &Path, name: &str, text: &str) {
        let tokens = lexer::tokens(text, Language::Js);
        for object in tokens.split(|t| t.text == "{").skip(1) {
            let field = |name: &str| {
                object
                    .windows(2)
                    .position(|p| {
                        p[0].text
                            .strip_prefix('"')
                            .and_then(|s| s.strip_suffix('"'))
                            == Some(name)
                            && p[1].text == ":"
                    })
                    .map(|i| i + 2)
            };
            let string = |name: &str| json_text(object.get(field(name)?)?);
            let (Some(dir), Some(file)) = (string("directory"), string("file")) else {
                continue;
            };
            let dir = root
                .join(Path::new(name).parent().unwrap_or(Path::new("")))
                .join(dir);
            // The database may use a symlink spelling for its working directory
            // (e.g. /var and /private/var on macOS). Resolve that directory once
            // before comparing captured repository-relative paths.
            let spelled = dir.clone();
            let dir = if dir.starts_with(root) {
                dir
            } else {
                dir.canonicalize().unwrap_or(dir)
            };
            let spelled_root = dir
                .strip_prefix(root)
                .ok()
                .and_then(|suffix| spelled.ancestors().nth(suffix.components().count()));
            let rebase = |path: &str| {
                spelled_root
                    .and_then(|base| Path::new(path).strip_prefix(base).ok())
                    .map(|relative| root.join(relative))
            };
            let file = rebase(&file).unwrap_or_else(|| PathBuf::from(file));
            let Some(file) = file.to_str().and_then(|file| inside(root, &dir, file)) else {
                continue;
            };
            let args = if let Some(i) =
                field("arguments").filter(|&i| object.get(i).is_some_and(|t| t.text == "["))
            {
                object[i + 1..]
                    .iter()
                    .take_while(|t| t.text != "]")
                    .filter(|t| t.text != ",")
                    .map(json_text)
                    .collect()
            } else {
                string("command").and_then(|s| split_command(&s))
            };
            let command = args.and_then(|mut args: Vec<String>| {
                // Rebase absolute include paths spelled under the same symlink
                // as the command directory, including joined -I options.
                if spelled != dir {
                    for arg in &mut args {
                        let (flag, value) = ["-iquote", "-isystem", "-include", "-I"]
                            .iter()
                            .find_map(|flag| arg.strip_prefix(flag).map(|v| (*flag, v)))
                            .unwrap_or(("", arg.as_str()));
                        if let Some(path) = rebase(value) {
                            *arg = format!("{}{}", flag, path.display());
                        }
                    }
                }
                Command::parse(root, &dir, &file, &args)
            });
            self.commands
                .entry(file)
                .and_modify(|prior| {
                    if *prior != command {
                        *prior = None;
                    }
                })
                .or_insert(command);
        }
    }
    pub(crate) fn analyze(&self, lang: Language, path: &str, text: &str) -> Analysis {
        let command = self.commands.get(path).and_then(Option::as_ref);
        let lang = command.map_or(
            lang,
            |cfg| if cfg.cpp { Language::Cpp } else { Language::C },
        );
        // Header analysis and repeated inclusions share the immutable token buffer.
        let header = matches!(
            Path::new(path).extension().and_then(|s| s.to_str()),
            Some("h" | "hh" | "hpp" | "hxx")
        )
        .then(|| self.header(path, lang == Language::Cpp))
        .flatten();
        let local;
        let tokens = if let Some((_, buf)) = &header {
            buf.as_ref()
        } else {
            local = lexer::tokens(text, lang);
            &local
        };
        let mut analyzer = super::Analyzer::new(lang, tokens);
        if let Some(command) = command {
            let paths = analyzer.project_directives(self, command, path);
            self.consulted
                .lock()
                .unwrap()
                .insert(path.to_owned(), paths);
        } else {
            analyzer.directives();
        }
        analyzer.declarations();
        analyzer.uses();
        analyzer.out
    }
    pub(crate) fn consulted(self) -> (HashMap<String, Vec<String>>, Vec<Receipt>) {
        (self.consulted.into_inner().unwrap(), self.sdk.receipts())
    }
    pub(crate) fn put_dependencies(
        analysis: &mut Analysis,
        deps: HashMap<String, Vec<String>>,
        receipts: Vec<Receipt>,
    ) {
        analysis.direct_evidence = Some(Box::new(super::super::direct::Evidence::CIncludes(
            deps, receipts,
        )));
    }
    pub(crate) fn take_dependencies(
        analysis: Analysis,
    ) -> Option<(HashMap<String, Vec<String>>, Vec<Receipt>)> {
        match *analysis.direct_evidence? {
            super::super::direct::Evidence::CIncludes(deps, receipts) => Some((deps, receipts)),
            _ => None,
        }
    }
    pub(super) fn sdk_proof(
        &self,
        name: &str,
        cmd: &Command,
        optional: bool,
        consulted: &mut std::collections::HashSet<String>,
    ) -> Option<Arc<Proof>> {
        let sdk_dirs: Vec<_> = cmd
            .includes
            .iter()
            .filter(|d| Path::new(d).is_absolute() && !cmd.opaque_dirs.contains(*d))
            .cloned()
            .collect();
        if sdk_dirs.is_empty() {
            return None;
        }
        let proof = self.sdk.proof(&sdk_dirs, name, optional);
        for receipt in self.sdk.receipts_for(&sdk_dirs, name, optional) {
            consulted.insert(receipt.key().to_owned());
        }
        let proof = proof?;
        let mut shadow = false;
        for dir in cmd
            .quote_dirs
            .iter()
            .chain(&cmd.includes)
            .filter(|dir| !Path::new(dir).is_absolute())
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
    pub(super) fn header(&self, path: &str, cpp: bool) -> Option<(&'a str, Arc<[Token<'a>]>)> {
        let (path, text) = (self.lookup)(path)?;
        let slot = Arc::clone(self.headers.lock().unwrap().entry((path, cpp)).or_default());
        let lang = if cpp { Language::Cpp } else { Language::C };
        Some((
            path,
            Arc::clone(slot.get_or_init(|| lexer::tokens(text, lang).into())),
        ))
    }
    pub(super) fn include(
        &self,
        file: &str,
        name: &str,
        quoted: bool,
        command: &Command,
        consulted: &mut std::collections::HashSet<String>,
    ) -> Result<Option<(&'a str, Arc<[Token<'a>]>)>, ()> {
        let dirs = Path::new(file)
            .parent()
            .filter(|_| quoted)
            .into_iter()
            .chain(command.quote_dirs.iter().filter(|_| quoted).map(Path::new))
            .chain(command.includes.iter().map(Path::new));
        let mut external = false;
        let mut opaque = false;
        for dir in dirs {
            if dir.is_absolute() || Path::new(name).is_absolute() {
                external = true;
                opaque |= dir
                    .to_str()
                    .is_some_and(|p| command.opaque_dirs.contains(p));
                continue;
            }
            let Some(path) = normalize(&dir.join(name)) else {
                continue;
            };
            let Some(path) = path.to_str() else { continue };
            consulted.insert(path.to_owned());
            if let Some(header) = self.header(path, command.cpp) {
                return if external { Err(()) } else { Ok(Some(header)) };
            }
        }
        if opaque { Err(()) } else { Ok(None) }
    }
}
