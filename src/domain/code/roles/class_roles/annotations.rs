//! Resolve Java and Swift test annotations from the imports written in this file.
use super::*;
const PACKAGES: &[&str] = &[
    "org.junit",
    "org.junit.jupiter.api",
    "org.junit.jupiter.params",
    "org.testng.annotations",
];

#[derive(Default)]
pub(super) struct Annotations<'a> {
    names: HashSet<&'a str>,
    wildcard: bool,
    swift_testing: bool,
}

impl<'a> Annotations<'a> {
    pub(super) fn new(lang: Language, s: &'a Syntax) -> Self {
        let imports = s
            .tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| token.text == "import")
            .map(|(i, _)| i + 1);
        match lang {
            Language::Swift => Self {
                swift_testing: imports.clone().any(|i| s.w(i) == "Testing"),
                ..Self::default()
            },
            Language::Java => Self::java(imports.filter_map(|i| java_member(s, i)).map(|i| s.w(i))),
            _ => Self::default(),
        }
    }

    fn java(imports: impl Iterator<Item = &'a str>) -> Self {
        let mut names: HashSet<_> = imports.collect();
        let wildcard = names.remove("*");
        Self {
            names,
            wildcard,
            swift_testing: false,
        }
    }
    pub(super) fn role(
        &self,
        lang: Language,
        s: &Syntax,
        start: usize,
        end: usize,
    ) -> Option<Role> {
        let simple = s.w(end);
        let role = annotation_role(simple)?;
        let imported = match lang {
            Language::Swift => simple == "Test" && self.swift_test(s, start, end),
            Language::Java if start != end => java_member(s, start) == Some(end),
            Language::Java => self.wildcard || self.names.contains(simple),
            _ => false,
        };
        imported.then_some(role)
    }

    fn swift_test(&self, s: &Syntax, start: usize, end: usize) -> bool {
        if start == end {
            return self.swift_testing;
        }
        end == start + 2 && s.w(start) == "Testing"
    }
}

fn annotation_role(name: &str) -> Option<Role> {
    Some(match name {
        "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory" | "TestTemplate" => {
            Role::Test
        }
        "Before" | "After" | "BeforeEach" | "AfterEach" | "BeforeAll" | "AfterAll" => Role::Support,
        _ => return None,
    })
}

fn java_member(s: &Syntax, start: usize) -> Option<usize> {
    for package in PACKAGES {
        let mut at = start;
        if package.split('.').all(|part| {
            let matches = s.w(at) == part && s.w(at + 1) == ".";
            at += 2;
            matches
        }) && s.w(at + 1) != "."
        {
            return Some(at);
        }
    }
    None
}
