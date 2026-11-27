//! Test methods belong to their declaring type. Resolve local inheritance
//! before considering conventions; nested and anonymous types own their bodies.
use super::{Kind, Language, Role, Roles, Syntax, lexer, script};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
enum Base {
    Framework,
    Named(String),
}

struct Type<'a> {
    keyword: usize,
    open: usize,
    close: usize,
    name: &'a str,
    base: Option<Base>,
    extension: bool,
}

fn is_word(s: &Syntax, i: usize) -> bool {
    s.tokens.get(i).is_some_and(|t| t.kind == Kind::Word)
}

pub(super) fn annotation_end(syntax: &Syntax, start: usize) -> usize {
    let mut end = start;
    while syntax.w(end + 1) == "." {
        end += 2;
    }
    end
}

mod types;
use types::types;

type Parents = HashMap<String, Option<Base>>;
fn parents(types: &[Type]) -> Parents {
    let mut out = Parents::new();
    for t in types.iter().filter(|t| !t.extension && !t.name.is_empty()) {
        out.entry(t.name.to_owned())
            .and_modify(|v| *v = None)
            .or_insert_with(|| t.base.clone());
    }
    out
}

#[derive(Default)]
pub(crate) struct Classes {
    siblings: HashMap<PathBuf, Parents>,
}

impl Classes {
    fn sibling_base(
        &mut self,
        lang: Language,
        path: &str,
        name: &str,
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<Option<Base>> {
        let ext = match lang {
            Language::Swift => "swift",
            Language::Java => "java",
            Language::Hack => "hack",
            _ => unreachable!(),
        };
        let key = Path::new(path).with_file_name(format!("{name}.{ext}"));
        let summary = match self.siblings.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let parents = read(entry.key())?.map_or_else(Parents::new, |text| {
                    let tokens = lexer::tokens(&text, lang);
                    let pairs = script::paired_for_score(&tokens);
                    let syntax = Syntax {
                        tokens: &tokens,
                        pairs: &pairs,
                        text: &text,
                    };
                    parents(&types(lang, &syntax))
                });
                entry.insert(parents)
            }
        };
        Ok(summary.get(name).cloned().flatten())
    }

    pub(super) fn collect(
        &mut self,
        file: &super::RolePath,
        s: &Syntax,
        res: &mut Roles,
        mut read: impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<()> {
        let (lang, path) = (file.lang, file.path);
        let declarations = types(lang, s);
        let mut inheritance = Inheritance {
            classes: self,
            lang,
            path,
            local: parents(&declarations),
            resolved: HashMap::new(),
            read: &mut read,
        };
        let mut suite_for = HashMap::new();
        for class in &declarations {
            suite_for.insert(class.open, inheritance.suite(class.suite_base())?);
        }
        let methods = Methods::new(lang, s);
        if lang == Language::Swift {
            // Swift Testing also registers free functions.
            methods.collect(0..s.tokens.len(), false, res);
        }
        let mut nesting = EnclosingTests::default();
        for t in &declarations {
            nesting.observe(s, t, res);
            methods.collect(t.open + 1..t.close, suite_for[&t.open], res);
        }
        Ok(())
    }
}

struct Inheritance<'a, R> {
    classes: &'a mut Classes,
    lang: Language,
    path: &'a str,
    local: Parents,
    resolved: HashMap<String, bool>,
    read: &'a mut R,
}

impl<R: FnMut(&Path) -> io::Result<Option<String>>> Inheritance<'_, R> {
    fn suite(&mut self, next: Option<Base>) -> io::Result<bool> {
        let mut visited = HashSet::new();
        let result = self.resolve_suite(next, &mut visited)?;
        self.resolved
            .extend(visited.into_iter().map(|name| (name, result)));
        Ok(result)
    }

    fn resolve_suite(
        &mut self,
        mut next: Option<Base>,
        visited: &mut HashSet<String>,
    ) -> io::Result<bool> {
        while let Some(Base::Named(name)) = next {
            let known = self
                .resolved
                .get(&name)
                .copied()
                .or_else(|| visited.contains(&name).then_some(false));
            if let Some(known) = known {
                return Ok(known);
            }
            next = self.base(&name)?;
            visited.insert(name);
        }
        Ok(matches!(next, Some(Base::Framework)))
    }

    fn base(&mut self, name: &str) -> io::Result<Option<Base>> {
        if let Some(base) = self.local.get(name) {
            return Ok(base.clone());
        }
        self.classes
            .sibling_base(self.lang, self.path, name, self.read)
    }
}

/// Parent methods can append regions as each type is visited. Only search
/// those regions for a containing test if one reaches the nested type's end.
#[derive(Default)]
struct EnclosingTests {
    checked: usize,
    farthest: usize,
}
impl EnclosingTests {
    fn observe(&mut self, s: &Syntax, declaration: &Type, res: &mut Roles) {
        let Type { keyword, close, .. } = *declaration;
        let start = s.tokens[keyword].span.start;
        let end = s.tokens[close].span.end;
        if self.contains(res, start, end) {
            s.add(
                res,
                keyword,
                Some(close),
                Role::Support.evidence("type declared inside a test"),
            );
        }
    }

    fn contains(&mut self, roles: &Roles, start: usize, end: usize) -> bool {
        for region in &roles.regions[self.checked..] {
            self.farthest = self.farthest.max(region.end);
        }
        self.checked = roles.regions.len();
        end <= self.farthest
            && roles
                .regions
                .iter()
                .rev()
                .find(|region| region.start <= start && end <= region.end)
                .is_some_and(|region| region.evidence.0 == Role::Test)
    }
}

impl Type<'_> {
    fn suite_base(&self) -> Option<Base> {
        if self.extension {
            Some(Base::Named(self.name.into()))
        } else {
            self.base.clone()
        }
    }
}

mod annotations;
use annotations::Annotations;

mod methods;
use methods::Methods;

#[cfg(test)]
mod tests {
    use super::*;
    fn check(
        (lang, path): (Language, &str),
        source: &str,
        siblings: &[(&str, &str)],
        expected: &[(&str, Role)],
    ) {
        use super::super::{Evidence, Role::*};
        let tokens = lexer::tokens(source, lang);
        let pairs = script::paired_for_score(&tokens);
        let syntax = Syntax {
            tokens: &tokens,
            pairs: &pairs,
            text: source,
        };
        let mut output = Roles {
            base: Evidence(Support, "fixture"),
            regions: Vec::new(),
        };
        Classes::default()
            .collect(
                &super::super::RolePath::new(path, lang),
                &syntax,
                &mut output,
                |p| {
                    Ok(siblings
                        .iter()
                        .find(|(name, _)| p == Path::new(name))
                        .map(|(_, text)| text.to_string()))
                },
            )
            .unwrap();
        output
            .regions
            .sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
        for &(needle, role) in expected {
            let pos = source.find(needle).unwrap();
            assert_eq!(
                output
                    .regions
                    .iter()
                    .rev()
                    .find(|r| r.start <= pos && pos < r.end)
                    .map_or(output.base.0, |r| r.evidence.0),
                role,
                "{needle}"
            );
        }
    }
    #[test]
    fn junit_uses_framework_and_anonymous_class_owns_its_methods() {
        use Role::*;
        check(
            (Language::Java, "Cases.java"),
            "import junit.framework.TestCase; import org.junit.Test; class Cases extends TestCase { public void testlower() { run(); Object o = new Base() { public void testFake() { inside(); } }; } public void testWrong(int x) { wrong(); } } class Plain { @Test public void registered() { yes(); } public void testSerialization() { dormant(); } }",
            &[],
            &[
                ("run()", Test),
                ("inside()", Support),
                ("wrong()", Support),
                ("yes()", Test),
                ("dormant()", Support),
            ],
        );
        check(
            (Language::Java, "Fake.java"),
            "import custom.Test; class Fake { @Test public void labeled() { nope(); } }",
            &[],
            &[("nope()", Support)],
        );
    }
    #[test]
    fn annotation_packages_are_exact_and_inheritance_cycles_are_local() {
        use Role::*;
        check(
            (Language::Java, "Methods.java"),
            "import org.junit.jupiter.api.*; class A extends B { public void testA() { cycle(); } } class B extends A {} class C { @Test void first() { imported(); } @org.junit.jupiter.params.ParameterizedTest void second() { qualified(); } @custom.org.junit.Test void third() { unrelated(); } }",
            &[],
            &[
                ("cycle()", Support),
                ("imported()", Test),
                ("qualified()", Test),
                ("unrelated()", Support),
            ],
        );
        check(
            (Language::Java, "Other.java"),
            "import custom.org.junit.Test; class Other { @Test void first() { unrelated(); } }",
            &[],
            &[("unrelated()", Support)],
        );
    }
    #[test]
    fn swift_extension_inherits_proven_sibling_superclass() {
        use Role::*;
        let code = "import XCTest\nclass Suite: Base {}\nextension Suite { func testlower() throws { pass() } func testArg(x: Int) { ignored() } }\nextension Production { func testMode() { ordinary() } }";
        check(
            (Language::Swift, "Cases/File.swift"),
            code,
            &[(
                "Cases/Base.swift",
                "import XCTest\nclass Base: XCTestCase {}",
            )],
            &[
                ("pass()", Test),
                ("ignored()", Support),
                ("ordinary()", Support),
            ],
        );
        check(
            (Language::Swift, "Cases/File.swift"),
            code,
            &[("Cases/Base.swift", "class Other {}")],
            &[("pass()", Support)],
        );
    }
    #[test]
    fn hack_trait_requires_a_test_case() {
        use Role::*;
        let code = "trait Helpers { require extends Base; <<DataProvider('cases')>> public function testInput<T as num>(T $x): void { run(); } } trait Normal { public function testApi(): void { normal(); } }";
        check(
            (Language::Hack, "tests/Something.hack"),
            code,
            &[(
                "tests/Base.hack",
                "abstract class Base extends \\Facebook\\HackTest\\HackTest {}",
            )],
            &[("run()", Test), ("normal()", Support)],
        );
    }
}
