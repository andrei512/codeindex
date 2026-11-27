//! Follow CTest registrations across linked CMake manifests.
use super::*;
use std::{cell::OnceCell, rc::Rc};

struct Call {
    name: String,
    args: Vec<String>,
    first: usize,
    last: usize,
}
#[derive(Default)]
struct Manifest {
    top: Vec<Call>,
    defs: HashMap<String, Definition>,
}

#[derive(Default)]
struct Definition {
    body: Vec<Call>,
    disabling_flags: OnceCell<HashSet<String>>,
}

impl Definition {
    fn disabled(&self, args: &[String]) -> bool {
        if args.is_empty() {
            return false;
        }
        let flags = self
            .disabling_flags
            .get_or_init(|| disabling_flags(&self.body));
        args.iter().any(|arg| flags.contains(arg))
    }
}

fn manifest(text: &str) -> Manifest {
    let mut manifest = Manifest::default();
    let commands = crate::domain::code::extended::build_langs::cmake::Commands::read(text);
    let offset = |word: &str| word.as_ptr() as usize - text.as_ptr() as usize;
    let mut calls = commands
        .iter()
        .map(|(name, args, close)| Call {
            name: name.to_ascii_lowercase(),
            args: args.iter().map(|arg| arg.text.to_owned()).collect(),
            first: offset(name),
            last: offset(close),
        })
        .peekable();
    while let Some(call) = calls.next() {
        match call.name.as_str() {
            "macro" | "function" => {
                let Some(name) = call.args.first() else {
                    continue;
                };
                let mut body = std::iter::from_fn(|| {
                    calls.next_if(|c| {
                        !matches!(
                            c.name.as_str(),
                            "macro" | "function" | "endmacro" | "endfunction"
                        )
                    })
                });
                if let Some(first) = body.next() {
                    let def = manifest.defs.entry(name.to_ascii_lowercase()).or_default();
                    def.body.push(first);
                    def.body.extend(body);
                }
            }
            "endmacro" | "endfunction" => {}
            _ => manifest.top.push(call),
        }
    }
    manifest
}
fn direct(c: &Call) -> bool {
    matches!(
        c.name.as_str(),
        "add_test" | "gtest_discover_tests" | "gtest_add_tests" | "catch_discover_tests"
    )
}
fn disabling_flags(body: &[Call]) -> HashSet<String> {
    // cmake_parse_arguments exposes a boolean option as <prefix>_<flag>.
    let guards: HashSet<_> = body
        .windows(3)
        .filter_map(|w| {
            if let [condition] = w[0].args.as_slice()
                && w[0].name == "if"
                && w[1].name == "return"
                && w[2].name == "endif"
            {
                Some(condition.as_str())
            } else {
                None
            }
        })
        .collect();
    let mut flags = HashSet::new();
    if guards.is_empty() {
        return flags;
    }
    for parse in body.iter().filter(|c| c.name == "cmake_parse_arguments") {
        let p = usize::from(parse.args.first().is_some_and(|a| a == "PARSE_ARGV")) * 2;
        if let (Some(prefix), Some(options)) = (parse.args.get(p), parse.args.get(p + 1)) {
            flags.extend(
                options
                    .split(';')
                    .filter(|flag| guards.contains(format!("{prefix}_{flag}").as_str()))
                    .map(str::to_owned),
            );
        }
    }
    flags
}
fn visit_registrations<'a>(
    call: &'a Call,
    defs: &HashMap<&'a str, &'a Definition>,
    mut visit: impl FnMut(&'a Call),
) -> bool {
    let mut visited = HashSet::new();
    let mut stack = Vec::new();
    let mut next = Some(call);
    let mut found = false;
    while let Some(c) = next.take().or_else(|| stack.pop()) {
        if direct(c) {
            visit(c);
            found = true;
            continue;
        }
        stack.extend(expand_registration(c, defs, &mut visited));
    }
    found
}

fn expand_registration<'a>(
    call: &'a Call,
    defs: &HashMap<&'a str, &'a Definition>,
    visited: &mut HashSet<&'a str>,
) -> &'a [Call] {
    let name = call.name.as_str();
    let Some(body) = defs.get(name) else {
        return &[];
    };
    if visited.contains(name) || body.disabled(&call.args) {
        return &[];
    }
    visited.insert(name);
    &body.body
}
fn executable(c: &Call) -> Option<&str> {
    let a = &c.args;
    if c.name == "add_test" {
        if a.first()?.eq_ignore_ascii_case("NAME") {
            return a
                .get(a.iter().position(|w| w == "COMMAND")? + 1)
                .map(String::as_str);
        }
        return a.get(1).map(String::as_str);
    }
    if direct(c) {
        a.first().map(String::as_str)
    } else {
        None
    }
}
#[derive(Default)]
pub(in crate::domain::code::roles) struct CTest {
    manifests: HashMap<PathBuf, Option<Rc<Manifest>>>,
    sources: HashMap<PathBuf, HashSet<PathBuf>>,
}

enum ManifestLink {
    Missing,
    Unlinked,
    Linked(Rc<Manifest>),
}

impl CTest {
    fn linked_manifest(
        &mut self,
        parent: &Path,
        child: Option<&Path>,
        read: &mut Reader<'_>,
    ) -> io::Result<ManifestLink> {
        let Some(manifest) = self.manifest(parent, read)? else {
            return Ok(ManifestLink::Missing);
        };
        if child.is_some_and(|child| !links_child(&manifest, parent, child)) {
            return Ok(ManifestLink::Unlinked);
        }
        Ok(ManifestLink::Linked(manifest))
    }

    fn manifest(&mut self, dir: &Path, read: &mut Reader<'_>) -> io::Result<Option<Rc<Manifest>>> {
        if let Some(manifest) = self.manifests.get(dir) {
            return Ok(manifest.clone());
        }
        let parsed = read(&dir.join("CMakeLists.txt"))?.map(|text| Rc::new(manifest(&text)));
        self.manifests.insert(dir.to_owned(), parsed.clone());
        Ok(parsed)
    }
    fn linked_manifests<'p>(
        &mut self,
        file: &'p Path,
        local: Option<Manifest>,
        read: &mut Reader<'_>,
    ) -> io::Result<Vec<(&'p Path, Rc<Manifest>)>> {
        let Some(dir) = file.parent() else {
            return Ok(Vec::new());
        };
        let mut context = Vec::new();
        if let Some(local) = local {
            context.push((dir, Rc::new(local)));
        }
        let mut ancestors = dir.ancestors();
        let mut child = if file.file_name().is_some_and(|n| n == "CMakeLists.txt") {
            ancestors.next()
        } else {
            None
        };
        for parent in ancestors {
            match self.linked_manifest(parent, child, read)? {
                ManifestLink::Missing => {}
                ManifestLink::Unlinked => break,
                ManifestLink::Linked(manifest) => {
                    context.push((parent, manifest));
                    child = Some(parent);
                }
            }
        }
        Ok(context)
    }

    pub(in crate::domain::code::roles) fn entry_points(
        &mut self,
        file: &Path,
        syntax: &Syntax,
        roles: &mut Roles,
        read: &mut Reader<'_>,
    ) -> io::Result<()> {
        if !syntax.tokens.iter().any(|t| t.text == "main") {
            return Ok(());
        }
        let Some(dir) = file.parent() else {
            return Ok(());
        };
        if !self.sources.contains_key(dir) {
            let context = self.linked_manifests(file, None, read)?;
            let mut defs = HashMap::new();
            let mut sources = HashSet::new();
            for (base, manifest) in context.iter().rev() {
                defs.extend(manifest.defs.iter().map(|(n, d)| (n.as_str(), d)));
                registered_sources(base, &manifest.top, &defs, &mut sources);
            }
            self.sources.insert(dir.to_owned(), sources);
        }
        if self.sources[dir].contains(file) {
            add_entry_points(syntax, roles);
        }
        Ok(())
    }
}
fn registered_sources(
    base: &Path,
    calls: &[Call],
    defs: &HashMap<&str, &Definition>,
    sources: &mut HashSet<PathBuf>,
) {
    let mut executed = HashSet::new();
    for call in calls {
        visit_registrations(call, defs, |registration| {
            executed.extend(executable(registration).map(executable_target));
        });
    }
    let inputs = calls
        .iter()
        .filter(|call| call.name == "add_executable")
        .filter_map(|call| call.args.split_first())
        .filter(|(target, _)| executed.contains(target.as_str()))
        .flat_map(|(_, inputs)| inputs);
    sources.extend(inputs.filter_map(|arg| script_path(base, arg)));
}

fn executable_target(command: &str) -> &str {
    command
        .strip_prefix("$<TARGET_FILE:")
        .and_then(|w| w.strip_suffix('>'))
        .unwrap_or(command)
}

fn links_child(manifest: &Manifest, parent: &Path, child: &Path) -> bool {
    manifest
        .top
        .iter()
        .filter(|call| call.name == "add_subdirectory")
        .filter_map(|call| script_path(parent, call.args.first()?))
        .any(|path| path == child)
}

fn invokes_script(call: &Call, base: &Path, file: &Path) -> bool {
    matches!(executable(call), Some("cmake" | "${CMAKE_COMMAND}"))
        && call
            .args
            .windows(2)
            .any(|args| args[0] == "-P" && script_path(base, &args[1]).as_deref() == Some(file))
}

impl CTest {
    pub(in crate::domain::code::roles) fn registrations(
        &mut self,
        file: &Path,
        s: &Syntax,
        out: &mut Roles,
        read: &mut Reader<'_>,
    ) -> io::Result<()> {
        let context = self.linked_manifests(file, Some(manifest(s.text)), read)?;
        let mut defs = HashMap::new();
        for (index, (base, manifest)) in context.iter().enumerate().rev() {
            defs.extend(manifest.defs.iter().map(|(n, d)| (n.as_str(), d)));
            for call in &manifest.top {
                let registered = visit_registrations(call, &defs, |registration| {
                    if invokes_script(registration, base, file) {
                        out.base = Evidence(Role::Test, "script invoked by CTest");
                    }
                });
                if registered && index == 0 {
                    s.add(
                        out,
                        s.tokens
                            .partition_point(|token| token.span.start < call.first),
                        Some(
                            s.tokens
                                .partition_point(|token| token.span.start < call.last),
                        ),
                        Role::Test.evidence("CTest registration"),
                    );
                }
            }
        }
        Ok(())
    }
}

fn add_entry_points(s: &Syntax, out: &mut Roles) {
    for i in 0..s.tokens.len() {
        if s.seq(i, &["main", "("])
            && let Some(close) = s.close(i + 1)
            && s.w(close + 1) == "{"
        {
            s.add(
                out,
                i,
                s.close(close + 1),
                Role::Test.evidence("entry point of a CTest executable"),
            );
        }
    }
}
