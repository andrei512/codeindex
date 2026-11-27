//! Navigation facts a language can supply and the identifiers exposed to clients.
use super::Language;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coverage {
    Unavailable,
    References,
    Calls,
}

#[derive(Clone, Copy)]
pub(crate) struct Navigation {
    pub pack: &'static str,
    pub coverage: Coverage,
    pub engine: Option<&'static str>,
    pub manifest: Option<&'static str>,
}

pub(crate) fn procedural_declaration(kind: &str) -> bool {
    matches!(kind, "plpgsql_function" | "plpgsql_procedure")
}

impl Language {
    /// A .sql file can expose procedural navigation. Other profiles need only the path.
    pub(crate) fn document_navigation<E>(
        self,
        path: &str,
        has_declaration: impl FnOnce(fn(&str) -> bool) -> Result<bool, E>,
    ) -> Result<Navigation, E> {
        let dialect = match self {
            Self::Sql if has_declaration(procedural_declaration)? => Self::PlPgSql,
            language => language,
        };
        Ok(dialect.navigation(path))
    }

    pub(crate) fn navigation(self, path: &str) -> Navigation {
        let mut profile = self.descriptor().navigation;
        if self == Self::Js {
            profile.pack = match Path::new(path).extension().and_then(|e| e.to_str()) {
                Some("tsx") => "typescriptreact",
                Some("jsx") => "javascriptreact",
                Some("ts" | "cts" | "mts") => "typescript",
                _ => profile.pack,
            };
        }
        profile
    }
}
