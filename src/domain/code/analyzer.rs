//! Dispatch at the document boundary. Syntax is borrowed only by the consumers
//! that requested it; plain symbol extraction never computes a score.
use super::comment_evidence::CommentEvidence;
use super::{
    Analysis, CProject, Token, cfamily, direct, extended, lexer, markup, rust, score, script,
};
use cfamily::analyzer::{CFamily, ConfiguredC};
use extended::analyzer::{ExtendedScripts, Objects};
use markup::analyzer::Sql;
use rust::analyzer::Rust;
use score::Input;

pub(crate) struct Measured {
    pub facts: Analysis,
    pub base: score::Saved,
    pub adjusted: score::Saved,
}

impl Measured {
    pub(crate) fn selected(self) -> score::Saved {
        self.adjusted.or(self.base)
    }
}

pub(crate) struct Document<'a> {
    pub path: &'a str,
    pub text: &'a str,
}

impl Document<'_> {
    /// Enter a token-based analyzer when the caller only has source text.
    /// Resumed indexing supplies its existing tokens through LexedAnalyzer directly.
    pub(super) fn tokenize_and_measure(
        &self,
        lang: Language,
        analyzer: &impl LexedAnalyzer,
        tests: bool,
    ) -> Measured {
        let tokens = lexer::tokens(self.text, lang);
        analyzer.from_tokens(self, &tokens, tests)
    }
}

pub(crate) trait ProjectInputs {
    type Context<'p>: FileAnalyzer
    where
        Self: 'p;
    fn prepare(&self, written: Language, path: &str) -> Option<Self::Context<'_>>;
}

pub(crate) trait ConfiguredScore: Sync {
    fn measure_saved(
        &self,
        doc: &Document,
        facts: &Analysis,
        configured: bool,
        tests: bool,
    ) -> score::Saved;
}

pub(crate) trait LexedAnalyzer: Sync {
    fn from_tokens(&self, doc: &Document, tokens: &[Token], tests: bool) -> Measured;
    fn resume_measure(&self, doc: &Document, state: &lexer::ScanState, tests: bool) -> Measured {
        self.from_tokens(doc, &state.tokens, tests)
    }
}

pub(crate) trait FileAnalyzer: Sync {
    fn comment_evidence(&self, lang: Language, text: &str) -> CommentEvidence {
        CommentEvidence::shared(lang, text)
    }
    fn configured_score(&self) -> Option<&dyn ConfiguredScore> {
        None
    }
    fn edit_proof(&self) -> Option<&dyn super::edit_proof::EditProof> {
        Some(&super::edit_proof::LocalNames)
    }
    fn lexed(&self) -> Option<&dyn LexedAnalyzer> {
        None
    }
    fn analyze(&self, doc: &Document) -> Analysis;
    fn measure(&self, doc: &Document, tests: bool) -> Measured;
    fn score(&self, doc: &Document, tests: bool) -> score::Saved {
        self.measure(doc, tests).selected()
    }
}

pub(super) struct Descriptor {
    pub resolver: Option<super::project_resolver::Provider>,
    pub analyzer: &'static dyn FileAnalyzer,
    pub navigation: super::navigation::Navigation,
    pub roles: super::roles::RoleSyntax,
}

use super::{navigation::Coverage, project_resolver::Provider, roles::RoleSyntax};

impl Descriptor {
    const fn new(
        analyzer: &'static dyn FileAnalyzer,
        pack: &'static str,
        coverage: Coverage,
        roles: RoleSyntax,
    ) -> Self {
        Self {
            analyzer,
            roles,
            resolver: None,
            navigation: super::navigation::Navigation {
                pack,
                coverage,
                engine: None,
                manifest: None,
            },
        }
    }

    const fn engine(mut self, name: &'static str) -> Self {
        self.navigation.engine = Some(name);
        self
    }

    const fn manifest(mut self, name: &'static str) -> Self {
        self.navigation.manifest = Some(name);
        self
    }

    const fn resolver(mut self, provider: Provider) -> Self {
        self.resolver = Some(provider);
        self
    }
}

// Generate dispatch and metadata in the same order from each language declaration.
macro_rules! registry {
    ($($language:ident $([$($suffix:literal)|+])? : $descriptor:expr),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Language { $($language),* }

        impl Language {
            const DESCRIPTORS: &'static [Descriptor] = &[$($descriptor),*];

            pub(super) fn for_suffix(suffix: &str) -> Option<Self> {
                match suffix {
                    $($( $($suffix)|+ => Some(Self::$language), )?)*
                    _ => None,
                }
            }

            pub(super) fn descriptor(self) -> &'static Descriptor {
                &Self::DESCRIPTORS[self as usize]
            }
        }
    };
}

impl Language {
    pub(crate) fn analyzer(self) -> &'static dyn FileAnalyzer {
        self.descriptor().analyzer
    }

    pub(crate) fn with_analyzer<T>(
        self,
        path: &str,
        project: Option<&CProject>,
        run: impl FnOnce(&dyn FileAnalyzer) -> T,
    ) -> T {
        if let Some(context) = project.and_then(|inputs| inputs.prepare(self, path)) {
            return run(&context);
        }
        run(self.analyzer())
    }
}

registry! {
    Rust ["rs"]: Descriptor::new(&Rust, "rust", Coverage::Calls, RoleSyntax::Registration)
        .manifest("Cargo.toml")
        .resolver(Provider::Rust),
    Js ["ts" | "tsx" | "cts" | "mts" | "js" | "jsx" | "mjs" | "cjs"]: Descriptor::new(script::analyzer::JAVASCRIPT, "javascript", Coverage::Calls, RoleSyntax::Javascript)
        .manifest("tsconfig.json")
        .resolver(Provider::Script),
    Python ["py" | "pyi"]: Descriptor::new(script::analyzer::PYTHON, "python", Coverage::Calls, RoleSyntax::Registration)
        .resolver(Provider::Script),
    Starlark ["bzl" | "bazel" | "BUILD" | "WORKSPACE"]: Descriptor::new(&Tokens(Language::Starlark, direct::starlark::analyze), "starlark", Coverage::Unavailable, RoleSyntax::Starlark)
        .resolver(Provider::Starlark),
    Shell ["sh" | "bash"]: Descriptor::new(&Source(Language::Shell, direct::analyze_shell, Comments::Native(|text, emit| { direct::shell_comments(text, emit); })), "shellscript", Coverage::Calls, RoleSyntax::Registration)
        .engine("bash-language-server")
        .resolver(Provider::Shell),
    Powershell ["ps1" | "psm1"]: Descriptor::new(&Tokens(Language::Powershell, direct::powershell::analyze), "powershell", Coverage::Calls, RoleSyntax::Registration)
        .engine("pwsh")
        .resolver(Provider::Powershell),
    Csharp ["cs"]: Descriptor::new(&CFamily(Language::Csharp), "csharp", Coverage::Calls, RoleSyntax::Registration)
        .resolver(Provider::Csharp),
    Swift ["swift"]: Descriptor::new(&CFamily(Language::Swift), "swift", Coverage::Calls, RoleSyntax::Class)
        .engine("sourcekit-lsp"),
    C ["c" | "h"]: Descriptor::new(&ConfiguredC(Language::C), "c", Coverage::Calls, RoleSyntax::Header)
        .engine("clangd"),
    Cpp ["cc" | "cpp" | "hpp" | "cxx"]: Descriptor::new(&ConfiguredC(Language::Cpp), "cpp", Coverage::Calls, RoleSyntax::Header)
        .engine("clangd"),
    Proto ["proto"]: Descriptor::new(&Tokens(Language::Proto, |_, tokens| direct::analyze_proto(tokens)), "proto", Coverage::References, RoleSyntax::Registration)
        .engine("protols")
        .resolver(Provider::Proto),
    Css ["css"]: Descriptor::new(&Tokens(Language::Css, |_, tokens| markup::css(tokens)), "css", Coverage::References, RoleSyntax::PathOnly)
        .engine("vscode-css-language-server"),
    Html ["html" | "htm"]: Descriptor::new(&markup::analyzer::Html, "html", Coverage::References, RoleSyntax::Html)
        .engine("vscode-html-language-server"),
    Sql ["sql"]: Descriptor::new(&Sql, "sql", Coverage::Unavailable, RoleSyntax::Registration),
    Go ["go"]: Descriptor::new(&Objects(Language::Go), "go", Coverage::Calls, RoleSyntax::Registration),
    Java ["java"]: Descriptor::new(&Objects(Language::Java), "java", Coverage::Calls, RoleSyntax::Class),
    Kotlin ["kt" | "kts"]: Descriptor::new(&Objects(Language::Kotlin), "kotlin", Coverage::Calls, RoleSyntax::Registration),
    ObjectiveC ["m"]: Descriptor::new(&Objects(Language::ObjectiveC), "objective-c", Coverage::Calls, RoleSyntax::Registration),
    Dart ["dart"]: Descriptor::new(&Objects(Language::Dart), "dart", Coverage::Calls, RoleSyntax::Registration),
    Php ["php" | "phtml"]: Descriptor::new(&Objects(Language::Php), "php", Coverage::Calls, RoleSyntax::Registration),
    Hack ["hack" | "hhi"]: Descriptor::new(&Objects(Language::Hack), "hack", Coverage::Calls, RoleSyntax::Class),
    Dockerfile ["dockerfile"]: Descriptor::new(&Source(Language::Dockerfile, extended::build_langs::dockerfile, Comments::Native(|text, emit| extended::docker_comments(text, emit))), "dockerfile", Coverage::Calls, RoleSyntax::PathOnly),
    Makefile ["mk" | "mak"]: Descriptor::new(&Source(Language::Makefile, extended::build_langs::makefile, Comments::Native(|text, emit| extended::make_comments(text, emit))), "makefile", Coverage::Calls, RoleSyntax::Registration),
    Batchfile ["cmd" | "bat"]: Descriptor::new(&Source(Language::Batchfile, extended::build_langs::batchfile, Comments::LineOriented), "batchfile", Coverage::Calls, RoleSyntax::Registration),
    CMake ["cmake"]: Descriptor::new(&Source(Language::CMake, extended::build_langs::cmake, Comments::Shared), "cmake", Coverage::Calls, RoleSyntax::Registration),
    Procfile: Descriptor::new(&Source(Language::Procfile, extended::build_langs::procfile, Comments::LineOriented), "procfile", Coverage::References, RoleSyntax::PathOnly),
    Hcl ["hcl" | "tf" | "tfvars"]: Descriptor::new(&Source(Language::Hcl, extended::build_langs::hcl, Comments::Native(|text, emit| extended::config_comments(text, false, emit))), "hcl", Coverage::Calls, RoleSyntax::Registration),
    Nix ["nix"]: Descriptor::new(&Source(Language::Nix, extended::build_langs::nix, Comments::Native(|text, emit| extended::config_comments(text, true, emit))), "nix", Coverage::Calls, RoleSyntax::Registration),
    Ruby ["rb" | "rake" | "gemspec" | "Gemfile" | "Rakefile"]: Descriptor::new(&extended::scripts::Ruby, "ruby", Coverage::Calls, RoleSyntax::Registration),
    Lua ["lua"]: Descriptor::new(&ExtendedScripts(Language::Lua), "lua", Coverage::Calls, RoleSyntax::Registration),
    Perl ["pl" | "pm"]: Descriptor::new(&ExtendedScripts(Language::Perl), "perl", Coverage::Calls, RoleSyntax::Registration),
    Assembly ["asm" | "s" | "S"]: Descriptor::new(&ExtendedScripts(Language::Assembly), "assembly", Coverage::Calls, RoleSyntax::PathOnly),
    Mako ["mako" | "mao"]: Descriptor::new(&Source(Language::Mako, |text| extended::mako_comments(text, |_| {}), Comments::Native(|text, emit| { extended::mako_comments(text, emit); })), "mako", Coverage::Calls, RoleSyntax::PathOnly),
    PlPgSql ["plpgsql" | "pgsql"]: Descriptor::new(&ExtendedScripts(Language::PlPgSql), "plpgsql", Coverage::Calls, RoleSyntax::Registration),
}

impl Input<'_, '_> {
    pub(super) fn measure(
        self,
        doc: &Document,
        facts: Analysis,
        base: score::Saved,
        tests: bool,
    ) -> Measured {
        let (base, adjusted) = self.policies(doc, base, Some(&facts), tests);
        Measured {
            facts,
            base,
            adjusted,
        }
    }
}
impl score::PairedSyntax<'_, '_> {
    pub(super) fn measure(
        self,
        lang: Language,
        doc: &Document,
        facts: Analysis,
        tests: bool,
    ) -> Measured {
        let (base, adjusted) = score::paired_scores(
            super::scoring::ScoreSource { lang, doc },
            self,
            &facts,
            tests,
        );
        Measured {
            facts,
            base,
            adjusted,
        }
    }
}

/// Native extractors consume either the captured source or its shared token stream.
enum Native {
    Source(Language, fn(&str) -> Analysis, Comments),
    Tokens(Language, fn(&str, &[Token]) -> Analysis),
}
use Native::{Source, Tokens};

enum Comments {
    Shared,
    Native(fn(&str, &mut dyn FnMut(usize))),
    LineOriented,
}

impl Native {
    fn extract(&self, text: &str, tokens: Option<&[Token]>) -> Analysis {
        match self {
            Source(_, extract, _) => extract(text),
            Tokens(lang, extract) => match tokens {
                Some(tokens) => extract(text, tokens),
                None => extract(text, &lexer::tokens(text, *lang)),
            },
        }
    }

    fn language(&self) -> Language {
        match self {
            Source(lang, _, _) | Tokens(lang, _) => *lang,
        }
    }
}

impl FileAnalyzer for Native {
    fn comment_evidence(&self, lang: Language, text: &str) -> CommentEvidence {
        match self {
            Source(_, _, Comments::Native(collect)) => CommentEvidence::native(lang, text, collect),
            Source(_, _, Comments::LineOriented) => CommentEvidence::line_oriented(lang, text),
            _ => CommentEvidence::shared(lang, text),
        }
    }

    fn lexed(&self) -> Option<&dyn LexedAnalyzer> {
        Some(self)
    }

    fn analyze(&self, doc: &Document) -> Analysis {
        self.extract(doc.text, None)
    }

    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        doc.tokenize_and_measure(self.language(), self, tests)
    }
}

impl LexedAnalyzer for Native {
    fn from_tokens(&self, doc: &Document, tokens: &[Token], tests: bool) -> Measured {
        let facts = self.extract(doc.text, Some(tokens));
        score::PairedSyntax {
            tokens,
            pairs: &script::paired_for_score(tokens),
        }
        .measure(self.language(), doc, facts, tests)
    }
}
