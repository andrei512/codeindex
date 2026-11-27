//! Resolve declarative test registrations using only files in the same snapshot.
use super::*;
use std::collections::{HashMap, HashSet};
use std::{
    io,
    path::{Path, PathBuf},
};

mod cmake;
pub(super) use cmake::CTest;
mod pgxs;
#[cfg(test)]
use pgxs::pg_regress;

#[derive(Default)]
pub(super) struct BuildRoles {
    pub(super) ctest: CTest,
    pgxs: pgxs::Pgxs,
    perl_distribution: Option<bool>,
}

type Reader<'a> = dyn FnMut(&Path) -> io::Result<Option<String>> + 'a;

impl BuildRoles {
    pub(super) fn perl_distribution(&mut self, read: &mut Reader<'_>) -> io::Result<bool> {
        if let Some(present) = self.perl_distribution {
            return Ok(present);
        }
        for manifest in ["dist.ini", "Makefile.PL", "cpanfile", "Build.PL"] {
            if read(Path::new(manifest))?.is_some() {
                self.perl_distribution = Some(true);
                return Ok(true);
            }
        }
        self.perl_distribution = Some(false);
        Ok(false)
    }
}

fn script_path(dir: &Path, arg: &str) -> Option<PathBuf> {
    let value = arg
        .strip_prefix("${CMAKE_CURRENT_SOURCE_DIR}/")
        .or_else(|| arg.strip_prefix("${CMAKE_CURRENT_LIST_DIR}/"))
        .unwrap_or(arg);
    if value.contains('$') {
        return None;
    }
    crate::domain::code::relative_path(dir, value)
}
enum MakeLine<'a> {
    Recipe(&'a str),
    Header,
    Trivia,
}

impl<'a> MakeLine<'a> {
    fn read(line: &'a str) -> Self {
        if let Some(command) = line.strip_prefix('\t') {
            return Self::Recipe(command);
        }
        let content = line.trim_start();
        if content.is_empty() || content.starts_with('#') {
            Self::Trivia
        } else {
            Self::Header
        }
    }
}

struct MakeTarget {
    region: Region,
    runs_tests: bool,
}

impl MakeTarget {
    fn header(line: &str, at: usize, number: usize) -> Option<Self> {
        let (target, rest) = line.split_once(':')?;
        if target.contains('=') || rest.starts_with('=') {
            return None;
        }
        Some(Self {
            region: Region {
                first: number,
                last: number,
                start: at,
                end: at + line.trim_end_matches('\n').len(),
                evidence: Evidence(Role::Support, "build target runs a test framework"),
            },
            runs_tests: rest
                .split_once(';')
                .is_some_and(|(_, cmd)| test_runner(cmd)),
        })
    }

    fn recipe(&mut self, line: &str, at: usize, number: usize, command: &str) {
        self.region.last = number;
        self.region.end = at + line.trim_end_matches('\n').len();
        self.runs_tests = self.runs_tests || test_runner(command);
    }

    fn emit(target: Option<Self>, out: &mut Roles) {
        out.regions.extend(
            target
                .filter(|target| target.runs_tests)
                .map(|target| target.region),
        );
    }
}

pub(super) fn make_targets(text: &str, out: &mut Roles) {
    let mut at = 0;
    let mut current: Option<MakeTarget> = None;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let number = i + 1;
        match MakeLine::read(line) {
            MakeLine::Recipe(command) => {
                if let Some(target) = &mut current {
                    target.recipe(line, at, number, command);
                }
            }
            MakeLine::Header => {
                MakeTarget::emit(current.take(), out);
                current = MakeTarget::header(line, at, number);
            }
            MakeLine::Trivia => {}
        }
        at += line.len();
    }
    MakeTarget::emit(current, out);
}

fn recipe_tool<'a>(rest: &mut &'a str) -> Option<std::borrow::Cow<'a, str>> {
    loop {
        let raw = rest.trim_start();
        let word = recipe_word(rest)?;
        let assignment = crate::domain::code::direct::shell_word::name_len(raw.as_bytes())
            .is_some_and(|end| raw.as_bytes().get(end) == Some(&b'='));
        if !assignment {
            return Some(word);
        }
    }
}

fn test_runner(cmd: &str) -> bool {
    let mut rest = cmd.trim_start().trim_start_matches(['@', '+', '-']);
    let Some(tool) = recipe_tool(&mut rest) else {
        return false;
    };
    match tool.rsplit('/').next().unwrap_or(&tool) {
        "go" | "cargo" | "dotnet" | "npm" | "bun" => {
            recipe_word(&mut rest).as_deref() == Some("test")
        }
        "pytest" | "ctest" | "prove" | "pg_prove" | "phpunit" | "jest" | "vitest" => true,
        "python" | "python3" => {
            recipe_word(&mut rest).as_deref() == Some("-m")
                && matches!(
                    recipe_word(&mut rest).as_deref(),
                    Some("pytest" | "unittest")
                )
        }
        _ => false,
    }
}

fn recipe_word<'a>(rest: &mut &'a str) -> Option<std::borrow::Cow<'a, str>> {
    use crate::domain::code::direct::shell_word::WordChars;
    let source = rest.trim_start();
    let mut word = WordChars::new(source);
    let decoded = word.decoded()?;
    *rest = &source[word.consumed()..];
    Some(decoded)
}

impl BuildRoles {
    pub(super) fn sql(
        &mut self,
        path: &str,
        s: &Syntax<'_>,
        out: &mut Roles,
        read: &mut Reader<'_>,
    ) -> io::Result<()> {
        if tap_regions(s, out) {
            out.base = Evidence(Role::Test, "pgTAP script with a plan and finish");
            return Ok(());
        }
        let file = Path::new(path);
        let Some(dir) = file
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n == "sql"))
        else {
            return Ok(());
        };
        for ancestor in dir.ancestors().skip(1) {
            let relative = file.strip_prefix(ancestor).unwrap().to_string_lossy();
            if self
                .pgxs
                .selects(ancestor.join("Makefile"), &relative, read)?
            {
                out.base = Evidence(Role::Test, "SQL case selected by PostgreSQL pg_regress");
                break;
            }
        }
        Ok(())
    }
}

fn select_call(s: &Syntax, i: usize) -> Option<(usize, bool)> {
    let mut f = i + 1;
    let from = s.w(f) == "*" && s.w(f + 1).eq_ignore_ascii_case("from");
    if from {
        f += 2
    }
    if s.w(f + 1) == "." {
        f += 2
    }
    if s.w(f + 1) != "(" {
        return None;
    }
    Some((f, from))
}

fn wrapper_end(s: &Syntax, statement: usize, function: usize) -> Option<usize> {
    if statement != 0 && s.w(statement - 1) != ";" {
        return None;
    }
    let end = s.close(function + 1)?;
    matches!(s.w(end + 1), ";" | "").then_some(end)
}

enum TapCall {
    Plan,
    Finish,
    Wrapper,
    Assertion,
}

impl TapCall {
    fn parse(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("plan") || name.eq_ignore_ascii_case("no_plan") {
            return Some(Self::Plan);
        }
        if name.eq_ignore_ascii_case("finish") {
            return Some(Self::Finish);
        }
        if name.eq_ignore_ascii_case("check_test") {
            return Some(Self::Wrapper);
        }
        ["ok", "is", "isnt", "throws_ok", "lives_ok", "results_eq"]
            .iter()
            .any(|a| name.eq_ignore_ascii_case(a))
            .then_some(Self::Assertion)
    }
}

pub(super) fn tap_regions(s: &Syntax, out: &mut Roles) -> bool {
    let mut script = TapScript::default();
    for (i, _) in s
        .tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.text.eq_ignore_ascii_case("select"))
    {
        script.select(s, i, out);
    }
    script.commit(s, out)
}

#[derive(Default)]
struct TapScript {
    plan: bool,
    finish: bool,
    wrappers: Vec<(usize, usize)>,
}

impl TapScript {
    const ASSERTION: Evidence = Evidence(Role::Test, "pgTAP assertion");

    fn select(&mut self, s: &Syntax, i: usize, out: &mut Roles) {
        let Some((kind, f, from)) =
            select_call(s, i).and_then(|(f, from)| Some((TapCall::parse(s.w(f))?, f, from)))
        else {
            return;
        };
        self.observe(kind, s, (i, f, from), out);
    }

    fn observe(&mut self, kind: TapCall, s: &Syntax, call: (usize, usize, bool), out: &mut Roles) {
        let (i, f, from) = call;
        match kind {
            TapCall::Plan => self.plan = true,
            TapCall::Finish => self.finish = true,
            TapCall::Wrapper => self.wrapper(s, i, f, from),
            TapCall::Assertion => Self::assertion(s, i, f, out),
        }
    }

    fn wrapper(&mut self, s: &Syntax, i: usize, f: usize, from: bool) {
        let inside = self.plan && !self.finish;
        if from && inside {
            self.wrappers
                .extend(wrapper_end(s, i, f).map(|end| (i, end)));
        }
    }

    fn assertion(s: &Syntax, i: usize, f: usize, out: &mut Roles) {
        if out.base.0 == Role::Support {
            s.add(out, i, s.close(f + 1), Self::ASSERTION);
        }
    }

    fn commit(self, s: &Syntax, out: &mut Roles) -> bool {
        if !self.plan || !self.finish {
            return false;
        }
        for (i, end) in self.wrappers {
            s.add(out, i, Some(end), Self::ASSERTION);
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn run(path: &str, lang: Language, text: &str, files: &[(&str, &str)]) -> Roles {
        let tokens = lexer::tokens(text, lang);
        let pairs = script::paired_for_score(&tokens);
        let result = Roles {
            base: Evidence(Role::Production, "ordinary"),
            regions: Vec::new(),
        };
        RolePath::new(path, lang)
            .collect_syntax(
                Syntax {
                    tokens: &tokens,
                    pairs: &pairs,
                    text,
                },
                result,
                &mut Context::default(),
                &mut |p| {
                    Ok(files
                        .iter()
                        .find(|(name, _)| Path::new(name) == p)
                        .map(|(_, source)| source.to_string()))
                },
            )
            .unwrap()
    }
    #[test]
    fn scoped_wrappers_label_invocations_and_registered_scripts() {
        let cmake = "function(register_case)\ncmake_parse_arguments(PARSE_ARGV 0 ARG \"SKIP\" \"\" \"\")\nif(ARG_SKIP)\nreturn()\nendif()\nadd_test(NAME demo COMMAND cmake -P \"${CMAKE_CURRENT_SOURCE_DIR}/verify.cmake\")\nendfunction()\nadd_subdirectory(sub)";
        let files = [
            ("CMakeLists.txt", cmake),
            ("sub/CMakeLists.txt", "register_case()"),
        ];
        let r = run(
            "sub/CMakeLists.txt",
            Language::CMake,
            "register_case()\nregister_case(SKIP)",
            &files,
        );
        assert_eq!(r.regions.len(), 1);
        assert_eq!(r.regions[0].first, 1);
        assert_eq!(
            run(
                "sub/verify.cmake",
                Language::CMake,
                "message(STATUS ok)",
                &files
            )
            .base
            .0,
            Role::Test
        );
        assert_eq!(
            run(
                "sub/helper.cmake",
                Language::CMake,
                "message(STATUS ok)",
                &files
            )
            .base
            .0,
            Role::Production
        );
    }
    #[test]
    fn only_entry_point_of_a_registered_executable_is_test() {
        let cmake = "add_executable(check sample.c)\nadd_executable(demo other.c)\nadd_test(NAME check-works COMMAND check)";
        let files = [("dir/CMakeLists.txt", cmake)];
        let source = "int util() {return 2;}\nint main(void) {return util();}";
        let r = run("dir/sample.c", Language::C, source, &files);
        assert_eq!(r.regions.len(), 1);
        assert_eq!(r.regions[0].first, 2);
        assert!(
            run("dir/other.c", Language::C, source, &files)
                .regions
                .is_empty()
        );
    }
    #[test]
    fn pgxs_requires_membership_and_input_directory() {
        let m = "include $(PGXS)\nTESTS = $(wildcard cases/sql/unit*.sql)\nREGRESS = fixed $(patsubst cases/sql/%.sql,%,$(TESTS))\nREGRESS_OPTS=--inputdir=cases";
        assert!(pg_regress(m, "cases/sql/unit1.sql"));
        assert!(pg_regress(m, "cases/sql/fixed.sql"));
        assert!(!pg_regress(m, "cases/sql/helper.sql"));
        assert!(!pg_regress(m, "other/sql/unit1.sql"));
        assert!(pg_regress("include $(PGXS)\nREGRESS=a b", "sql/b.sql"));
        assert!(!pg_regress(
            "include $(PGXS)\nX=$(X)$(X)$(X)\nREGRESS=$(X)",
            "sql/no.sql"
        ));
    }
    #[test]
    fn distinguishes_registered_tap_script_from_literals() {
        assert_eq!(
            run(
                "check.sql",
                Language::Sql,
                "SELECT plan(1); SELECT * FROM finish();",
                &[]
            )
            .base
            .0,
            Role::Test
        );
        assert_eq!(
            run(
                "q.sql",
                Language::Sql,
                "SELECT 'SELECT plan(1)'; SELECT * FROM finish();",
                &[]
            )
            .base
            .0,
            Role::Production
        )
    }
    #[test]
    fn make_target_scope_preserves_neighboring_targets() {
        let m = "build:\n\t@echo go test\ncheck:\n\tgo test ./...\nall: check\n";
        let r = run("Makefile", Language::Makefile, m, &[]);
        assert_eq!(r.regions.len(), 1);
        assert_eq!((r.regions[0].first, r.regions[0].last), (3, 4));
        assert_eq!(r.regions[0].evidence.0, Role::Support);
    }
}
#[cfg(test)]
#[test]
fn manifests_read_by_a_shared_role_query() {
    use std::collections::BTreeMap;
    let root = "add_subdirectory(sub)\nfunction(run_case)\nadd_test(NAME demo COMMAND checked)\nendfunction()";
    let local = "add_executable(checked first.c second.c)\nrun_case()";
    let mut reads: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut context = super::Context::default();
    for name in ["sub/first.c", "sub/second.c", "sub/other.c"] {
        let roles = super::classify_with(name, "int main(){return 0;}", &mut context, |p| {
            *reads.entry(p.to_owned()).or_default() += 1;
            Ok(match p.to_str().unwrap() {
                "sub/CMakeLists.txt" => Some(local.into()),
                "CMakeLists.txt" => Some(root.into()),
                _ => None,
            })
        })
        .unwrap();
        assert_eq!(roles.regions.len(), usize::from(!name.ends_with("other.c")));
    }
    eprintln!("manifest reads {reads:?}");
    assert_eq!(reads[Path::new("CMakeLists.txt")], 1);
    assert_eq!(reads[Path::new("sub/CMakeLists.txt")], 1);
}
