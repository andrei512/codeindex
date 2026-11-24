//! Native language profiles share the existing source-span and per-file evidence contracts.
use super::{Analysis, Language};
mod objects;
mod build_langs;
mod scripts;

impl Language {
    pub(crate) fn extended_name(self) -> Option<&'static str> {
        Some(match self {
            Self::Go => "go",
            Self::Java => "java",
            Self::Kotlin => "kotlin",
            Self::ObjectiveC => "objective-c",
            Self::Dart => "dart",
            Self::Php => "php",
            Self::Hack => "hack",
            Self::Dockerfile => "dockerfile",
            Self::Makefile => "makefile",
            Self::Batchfile => "batchfile",
            Self::CMake => "cmake",
            Self::Procfile => "procfile",
            Self::Hcl => "hcl",
            Self::Nix => "nix",
            Self::Ruby => "ruby",
            Self::Lua => "lua",
            Self::Perl => "perl",
            Self::Assembly => "assembly",
            Self::Mako => "mako",
            Self::PlPgSql => "plpgsql",
            _ => return None,
        })
    }
}

pub(super) fn analyze(lang: Language, source: &str) -> Option<Analysis> {
    use Language::*;
    Some(match lang {
        Go | Java | Kotlin | ObjectiveC | Dart | Php | Hack => objects::analyze(lang, source),
        Dockerfile | Makefile | Batchfile | CMake | Procfile | Hcl | Nix => build_langs::analyze(lang, source),
        Ruby | Lua | Perl | Assembly | Mako | PlPgSql => scripts::analyze(lang, source),
        _ => return None,
    })
}

pub(super) fn sql_routines(source: &str, facts: &mut Analysis) {
    scripts::sql_routines(source, facts);
}
