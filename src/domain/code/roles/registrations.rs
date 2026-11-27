use super::{Language, Role, Roles, Syntax};
use Language::*;
use Role::{Support, Test};

fn test_name(name: &str) -> bool {
    name.strip_prefix("test")
        .is_some_and(|s| s.starts_with('_') || s.starts_with(char::is_uppercase))
}

fn has_test_framework(lang: Language, s: &Syntax) -> bool {
    match lang {
        Dart => s.tokens.windows(2).any(|t| {
            t[0].text == "import"
                && matches!(
                    t[1].text.trim_matches(['\'', '"']),
                    "package:test/test.dart" | "package:flutter_test/flutter_test.dart"
                )
        }),
        Java | Kotlin => s
            .tokens
            .iter()
            .enumerate()
            .any(|(i, token)| jvm_framework(s, i, token.text)),
        Csharp => s
            .tokens
            .iter()
            .any(|t| matches!(t.text, "Xunit" | "NUnit" | "TestTools")),
        Swift => s.tokens.iter().any(|t| t.text == "Testing"),
        _ => false,
    }
}

fn jvm_framework(s: &Syntax, i: usize, name: &str) -> bool {
    match name {
        "junit" | "testng" | "kotest" => true,
        "kotlin" => s.seq(i + 1, &[".", "test"]),
        _ => false,
    }
}

mod attributes;
mod callback;
mod dart;
mod go;
mod kotest;
mod perl;
mod pester;
mod ruby;
mod rust;
pub(super) use callback::callback_body;
pub(super) use kotest::kotest_body;
pub(super) use perl::{perl_body, subtest};
pub(super) use pester::pester_body;
pub(in crate::domain::code) use ruby::RubyBlocks;
mod scripts;

pub(super) fn collect(
    file: &super::RolePath,
    s: &Syntax,
    res: &mut Roles,
    ruby: &std::cell::OnceCell<RubyBlocks>,
) {
    let (lang, path) = (file.lang, file.path);
    match lang {
        Go => go::go(path, s, res),
        Python => super::python::collect(path, s, res),
        Java | Kotlin | Swift => attributes::annotations(lang, s, res),
        ObjectiveC | Php => attributes::suite_methods(lang, s, res),
        Ruby => ruby::collect(s, res, ruby),
        Hcl => scripts::hcl(path, s, res),
        _ => collect_local(lang, s, res),
    }
}

fn collect_local(lang: Language, s: &Syntax, res: &mut Roles) {
    match lang {
        Rust => rust::rust(s, res),
        Csharp => attributes::csharp(s, res),
        Dart => dart::dart(s, res),
        Lua | Powershell | Shell | Perl => collect_script(lang, s, res),
        _ => {}
    }
}

fn collect_script(lang: Language, s: &Syntax, res: &mut Roles) {
    match lang {
        Lua => scripts::lua(s, res),
        Powershell => pester::collect(s, res),
        Shell => scripts::shell(s, res),
        Perl => perl::subtests(s, res),
        _ => {}
    }
}
