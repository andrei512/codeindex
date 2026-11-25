//! Resolve declarative test registrations using only files in the same snapshot.
use super::*;
use std::collections::{HashMap, HashSet};
use std::{
    io,
    path::{Path, PathBuf},
};

type Reader<'a> = dyn FnMut(&Path) -> io::Result<Option<String>> + 'a;

pub(super) fn collect(
    lang: Language,
    path: &str,
    s: &Syntax<'_>,
    out: &mut Roles,
    read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
) -> io::Result<()> {
    match lang {
        Language::CMake => cmake(path, s, out, read, false)?,
        Language::C | Language::Cpp if s.tokens.iter().any(|t| t.text == "main") => {
            cmake(path, s, out, read, true)?
        }
        Language::Makefile => make_targets(s.text, out),
        Language::Sql | Language::PlPgSql => sql(path, s, out, read)?,
        _ => {}
    }
    Ok(())
}

#[derive(Clone)]
struct Call {
    name: String,
    args: Vec<String>,
    first: usize,
    last: usize,
}
fn calls(s: &Syntax<'_>) -> Vec<Call> {
    let mut calls = Vec::new();
    let mut i = 0;
    while i + 1 < s.tokens.len() {
        if s.w(i + 1) != "(" {
            i += 1;
            continue;
        }
        let Some(last) = s.close(i + 1) else {
            i += 1;
            continue;
        };
        let mut args = Vec::new();
        let mut begin = i + 2;
        while begin < last {
            let mut end = begin;
            while end + 1 < last && s.tokens[end].span.end == s.tokens[end + 1].span.start {
                end += 1
            }
            args.push(
                s.text[s.tokens[begin].span.start..s.tokens[end].span.end]
                    .trim_matches('"')
                    .to_owned(),
            );
            begin = end + 1;
        }
        calls.push(Call {
            name: s.w(i).to_ascii_lowercase(),
            args,
            first: i,
            last,
        });
        i = last + 1;
    }
    calls
}
#[derive(Default)]
struct Manifest {
    top: Vec<Call>,
    defs: HashMap<String, Vec<Call>>,
}
fn manifest(calls: Vec<Call>) -> Manifest {
    let mut m = Manifest::default();
    let mut def = None;
    for c in calls {
        match c.name.as_str() {
            "macro" | "function" => def = c.args.first().map(|a| a.to_ascii_lowercase()),
            "endmacro" | "endfunction" => def = None,
            _ => {
                if let Some(name) = &def {
                    m.defs.entry(name.clone()).or_default().push(c)
                } else {
                    m.top.push(c)
                }
            }
        }
    }
    m
}
fn direct(c: &Call) -> bool {
    matches!(
        c.name.as_str(),
        "add_test" | "gtest_discover_tests" | "gtest_add_tests" | "catch_discover_tests"
    )
}
fn disabled(args: &[String], body: &[Call]) -> bool {
    // cmake_parse_arguments prefixes each boolean option. Honor a simple unconditional return for that option.
    for parse in body.iter().filter(|c| c.name == "cmake_parse_arguments") {
        let p = usize::from(parse.args.first().is_some_and(|a| a == "PARSE_ARGV")) * 2;
        let (Some(prefix), Some(options)) = (parse.args.get(p), parse.args.get(p + 1)) else {
            continue;
        };
        for flag in options.split(';').filter(|f| args.iter().any(|a| a == f)) {
            if body.windows(3).any(|w| {
                w[0].name == "if"
                    && w[0].args == [format!("{prefix}_{flag}")]
                    && w[1].name == "return"
                    && w[2].name == "endif"
            }) {
                return true;
            }
        }
    }
    false
}
fn registrations<'a>(call: &'a Call, defs: &'a HashMap<String, Vec<Call>>) -> Vec<&'a Call> {
    if direct(call) {
        return vec![call];
    }
    if !defs.contains_key(&call.name) {
        return Vec::new();
    }
    let mut stack = vec![call];
    let mut visited = HashSet::new();
    let mut found = Vec::new();
    while let Some(c) = stack.pop() {
        if direct(c) {
            found.push(c);
            continue;
        }
        if let Some(body) = defs.get(&c.name)
            && !visited.contains(&c.name)
            && !disabled(&c.args, body)
        {
            visited.insert(&c.name);
            stack.extend(body);
        }
    }
    found
}
fn script_path(dir: &Path, arg: &str) -> Option<PathBuf> {
    let value = arg
        .strip_prefix("${CMAKE_CURRENT_SOURCE_DIR}/")
        .or_else(|| arg.strip_prefix("${CMAKE_CURRENT_LIST_DIR}/"))
        .unwrap_or(arg);
    if value.contains('$') || Path::new(value).is_absolute() {
        return None;
    }
    let mut p = dir.to_path_buf();
    for part in Path::new(value).components() {
        match part {
            std::path::Component::Normal(x) => p.push(x),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if p.pop() => {}
            _ => return None,
        }
    }
    Some(p)
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
fn cmake(
    path: &str,
    s: &Syntax<'_>,
    out: &mut Roles,
    read: &mut Reader<'_>,
    c_source: bool,
) -> io::Result<()> {
    let file = Path::new(path);
    let current = file.file_name().is_some_and(|n| n == "CMakeLists.txt");
    let own = if c_source {
        Manifest::default()
    } else {
        manifest(calls(s))
    };
    let Some(dir) = file.parent() else {
        return Ok(());
    };
    let mut context = Vec::new();
    let mut child = None;
    for parent in dir.ancestors() {
        let lists = parent.join("CMakeLists.txt");
        if current && parent == dir {
            child = Some(parent);
            continue;
        }
        let Some(text) = read(&lists)? else { continue };
        let tokens = lexer::tokens(&text, Language::CMake);
        let pairs = script::paired_for_score(&tokens);
        let m = manifest(calls(&Syntax {
            tokens: &tokens,
            pairs: &pairs,
            text: &text,
        }));
        if child.is_some_and(|ch| {
            !m.top.iter().any(|c| {
                c.name == "add_subdirectory"
                    && c.args
                        .first()
                        .and_then(|a| script_path(parent, a))
                        .as_deref()
                        == Some(ch)
            })
        }) {
            break;
        }
        context.push((parent.to_path_buf(), m));
        child = Some(parent);
    }
    context.insert(0, (dir.to_path_buf(), own));
    let mut defs = HashMap::new();
    for (index, (base, m)) in context.iter_mut().enumerate().rev() {
        defs.extend(m.defs.drain());
        let mut executed = HashSet::new();
        for c in &m.top {
            let found = registrations(c, &defs);
            if found.is_empty() {
                continue;
            }
            if index == 0 && !c_source {
                s.add(out, c.first, Some(c.last), Role::Test, "CTest registration")
            }
            for f in found {
                if let Some(e) = executable(f) {
                    executed.insert(
                        e.strip_prefix("$<TARGET_FILE:")
                            .and_then(|w| w.strip_suffix('>'))
                            .unwrap_or(e),
                    );
                }
                if !c_source
                    && matches!(executable(f), Some("cmake" | "${CMAKE_COMMAND}"))
                    && f.args
                        .windows(2)
                        .any(|a| a[0] == "-P" && script_path(base, &a[1]).as_deref() == Some(file))
                {
                    out.base = Evidence(Role::Test, "script invoked by CTest");
                }
            }
        }
        if c_source
            && m.top.iter().any(|c| {
                c.name == "add_executable"
                    && c.args
                        .first()
                        .is_some_and(|t| executed.contains(t.as_str()))
                    && c.args
                        .iter()
                        .skip(1)
                        .any(|a| script_path(base, a).as_deref() == Some(file))
            })
        {
            for i in 0..s.tokens.len() {
                if s.seq(i, &["main", "("])
                    && let Some(close) = s.close(i + 1)
                    && s.w(close + 1) == "{"
                {
                    s.add(
                        out,
                        i,
                        s.close(close + 1),
                        Role::Test,
                        "entry point of a CTest executable",
                    );
                }
            }
            break;
        }
    }
    Ok(())
}

fn make_targets(text: &str, out: &mut Roles) {
    let mut at = 0;
    let mut current: Option<(usize, usize, usize, usize, bool)> = None;
    let emit = |target: Option<(usize, usize, usize, usize, bool)>, out: &mut Roles| {
        if let Some((first, last, start, end, true)) = target {
            out.regions.push(Region {
                first,
                last,
                start,
                end,
                evidence: Evidence(Role::Support, "build target runs a test framework"),
            })
        }
    };
    for (n, line) in text.split_inclusive('\n').enumerate() {
        if let Some(cmd) = line.strip_prefix('\t') {
            if let Some((_, last, _, end, runs)) = &mut current {
                *last = n + 1;
                *end = at + line.trim_end_matches('\n').len();
                *runs |= test_runner(cmd);
            }
        } else if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            emit(current.take(), out);
            if let Some((target, rest)) = line.split_once(':')
                && !target.contains('=')
                && !rest.starts_with('=')
            {
                current = Some((
                    n + 1,
                    n + 1,
                    at,
                    at + line.trim_end_matches('\n').len(),
                    rest.split_once(';')
                        .is_some_and(|(_, cmd)| test_runner(cmd)),
                ));
            }
        }
        at += line.len();
    }
    emit(current, out);
}
fn test_runner(cmd: &str) -> bool {
    let mut a = cmd
        .trim_start()
        .trim_start_matches(['@', '+', '-'])
        .split_whitespace()
        .skip_while(|v| {
            v.split_once('=').is_some_and(|(k, _)| {
                !k.is_empty() && k.bytes().all(|c| c == b'_' || c.is_ascii_alphanumeric())
            })
        });
    let Some(tool) = a.next() else { return false };
    let arg = a.next();
    match tool.rsplit('/').next().unwrap_or(tool) {
        "go" | "cargo" | "dotnet" | "npm" | "bun" => arg == Some("test"),
        "pytest" | "ctest" | "prove" | "pg_prove" | "phpunit" | "jest" | "vitest" => true,
        "python" | "python3" => {
            arg == Some("-m") && matches!(a.next(), Some("pytest" | "unittest"))
        }
        _ => false,
    }
}

fn sql(path: &str, s: &Syntax<'_>, out: &mut Roles, read: &mut Reader<'_>) -> io::Result<()> {
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
        let Some(make) = read(&ancestor.join("Makefile"))? else {
            continue;
        };
        let relative = file.strip_prefix(ancestor).unwrap().to_string_lossy();
        if pg_regress(&make, &relative) {
            out.base = Evidence(Role::Test, "SQL case selected by PostgreSQL pg_regress");
            break;
        }
    }
    Ok(())
}

pub(super) fn tap_regions(s: &Syntax, out: &mut Roles) -> bool {
    let (mut plan, mut finish) = (false, false);
    let mut wrappers = Vec::new();
    for i in 0..s.tokens.len() {
        if !s.w(i).eq_ignore_ascii_case("select") {
            continue;
        }
        let mut f = i + 1;
        if s.w(f) == "*" && s.w(f + 1).eq_ignore_ascii_case("from") {
            f += 2
        }
        if s.w(f + 1) == "." {
            f += 2
        }
        if s.w(f + 1) != "(" {
            continue;
        }
        match s.w(f).to_ascii_lowercase().as_str() {
            "plan" | "no_plan" => plan = true,
            "finish" => finish = true,
            "check_test" if plan && !finish && (i == 0 || s.w(i - 1) == ";")
                && s.w(i + 1) == "*" && s.w(i + 2).eq_ignore_ascii_case("from") => {
                if let Some(end) = s.close(f + 1)
                    && matches!(s.w(end + 1), ";" | "") {
                    wrappers.push((i, end));
                }
            }
            "ok" | "is" | "isnt" | "throws_ok" | "lives_ok" | "results_eq"
                if out.base.0 == Role::Support =>
            {
                s.add(out, i, s.close(f + 1), Role::Test, "pgTAP assertion")
            }
            _ => {}
        }
    }
    if plan && finish {
        for (i, end) in wrappers {
            s.add(out, i, Some(end), Role::Test, "pgTAP assertion");
        }
    }
    plan && finish
}

// Evaluate the small, side-effect-free subset of GNU make used to select a SQL case.
// Wildcard membership is evaluated for this one existing path; no directory listing is needed.
fn make_value(value: &str, vars: &HashMap<&str, String>, file: &str) -> String {
    let mut steps = 128;
    let result = expand_make_value(value, vars, file, &mut steps);
    if steps == 0 { String::new() } else { result }
}
fn expand_make_value(
    value: &str,
    vars: &HashMap<&str, String>,
    file: &str,
    steps: &mut usize,
) -> String {
    if *steps == 0 {
        return String::new();
    }
    *steps -= 1;
    let mut tail = value;
    let mut result = String::new();
    while let Some((prefix, rest)) = tail.split_once("$(") {
        result.push_str(prefix);
        if *steps == 0 {
            return String::new();
        }
        let mut level = 1;
        let Some(end) = rest.char_indices().find_map(|(at, ch)| {
            match ch {
                '(' => level += 1,
                ')' => level -= 1,
                _ => {}
            }
            (level == 0).then_some(at)
        }) else {
            return String::new();
        };
        let expr = &rest[..end];
        let v = if let Some(args) = expr.strip_prefix("wildcard ") {
            let args = expand_make_value(args, vars, file, steps);
            if args.split_whitespace().any(|pat| {
                pat.split_once('*').is_some_and(|(a, b)| {
                    file.strip_prefix(a)
                        .and_then(|part| part.strip_suffix(b))
                        .is_some_and(|middle| !middle.contains('/'))
                }) || pat == file
            }) {
                file.to_owned()
            } else {
                String::new()
            }
        } else if let Some(args) = expr.strip_prefix("patsubst ") {
            let mut parts = args.splitn(3, ',');
            let (Some(pattern), Some(replacement), Some(list)) =
                (parts.next(), parts.next(), parts.next())
            else {
                return String::new();
            };
            expand_make_value(list, vars, file, steps)
                .split_whitespace()
                .map(|v| {
                    if let Some((a, b)) = pattern.split_once('%')
                        && let Some(stem) = v.strip_prefix(a).and_then(|v| v.strip_suffix(b))
                    {
                        replacement.replace('%', stem)
                    } else {
                        v.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            vars.get(expr)
                .map(|v| expand_make_value(v, vars, file, steps))
                .unwrap_or_default()
        };
        result.push_str(&v);
        tail = &rest[end + 1..];
    }
    result.push_str(tail);
    result
}
fn pg_regress(make: &str, file: &str) -> bool {
    let make = make.replace("\\\n", " ");
    if !make.lines().map(str::trim).any(|l| {
        l.strip_prefix("include ").is_some_and(|v| {
            v.split_whitespace()
                .any(|v| v == "$(PGXS)" || v.ends_with("/pgxs.mk"))
        })
    }) {
        return false;
    }
    let mut vars: HashMap<&str, String> = HashMap::new();
    for line in make.lines().filter(|l| !l.starts_with('\t')) {
        let line = line.split('#').next().unwrap_or("");
        if let Some((lhs, rhs)) = line.split_once('=') {
            let lhs = lhs.trim();
            let name = lhs.trim_end_matches([':', '+', '?']).trim();
            if name.is_empty() || name.contains(char::is_whitespace) {
                continue;
            }
            if lhs.ends_with('+') {
                let v = vars.entry(name).or_default();
                v.push(' ');
                v.push_str(rhs.trim())
            } else if !lhs.ends_with('?') || !vars.contains_key(name) {
                vars.insert(name, rhs.trim().to_owned());
            }
        }
    }
    let Some(regress) = vars.get("REGRESS") else {
        return false;
    };
    let opts = make_value(
        vars.get("REGRESS_OPTS").map_or("", String::as_str),
        &vars,
        file,
    );
    let mut words = opts.split_whitespace();
    let mut dir = ".";
    while let Some(opt) = words.next() {
        if let Some(d) = opt.strip_prefix("--inputdir=") {
            dir = d
        } else if opt == "--inputdir" {
            dir = words.next().unwrap_or(".")
        }
    }
    let Some(folder) = script_path(Path::new(""), dir).map(|d| d.join("sql")) else {
        return false;
    };
    make_value(regress, &vars, file)
        .split_whitespace()
        .any(|case| folder.join(format!("{case}.sql")) == Path::new(file))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(path: &str, lang: Language, text: &str, files: &[(&str, &str)]) -> Roles {
        let tokens = lexer::tokens(text, lang);
        let pairs = script::paired_for_score(&tokens);
        let mut result = Roles {
            base: Evidence(Role::Production, "ordinary"),
            regions: Vec::new(),
        };
        collect(
            lang,
            path,
            &Syntax {
                tokens: &tokens,
                pairs: &pairs,
                text,
            },
            &mut result,
            &mut |p| {
                Ok(files
                    .iter()
                    .find(|(n, _)| Path::new(n) == p)
                    .map(|(_, s)| s.to_string()))
            },
        )
        .unwrap();
        result
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
