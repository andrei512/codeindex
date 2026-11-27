//! Operand-taking flags. Each registration supplies both the clap grammar
//! and the action executed when the caller or watcher interprets that flag.
use super::*;

pub(crate) struct ValueOption {
    pub name: &'static str,
    pub short: char,
    missing: Option<&'static str>,
    set: Apply,
}

impl ValueOption {
    pub(crate) fn named(name: &str) -> Option<&'static Self> {
        Self::ALL.iter().find(|option| option.name == name)
    }

    pub(crate) fn parse(flag: &str) -> Option<&'static Self> {
        Self::named(flag.strip_prefix("--")?)
    }

    pub(crate) fn missing(&self, spelling: &str) -> String {
        self.missing
            .map(str::to_owned)
            .unwrap_or_else(|| format!("missing argument for {spelling}"))
    }

    pub(super) fn apply<'a>(
        &self,
        options: &mut Options<'a>,
        value: &'a str,
        spelling: &str,
    ) -> Result<(), String> {
        (self.set)(options, value, spelling)
    }
}

fn number(value: &str, flag: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("invalid number for {flag}"))
}

fn named_number(value: &str, name: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {name}: {value}"))
}

const COLOR_CHOICES: &str = "expected auto, always, ansi or never for --color";

type Apply = for<'a> fn(&mut Options<'a>, &'a str, &str) -> Result<(), String>;

const fn option(
    name: &'static str,
    short: char,
    missing: Option<&'static str>,
    set: Apply,
) -> ValueOption {
    ValueOption {
        name,
        short,
        missing,
        set,
    }
}

impl ValueOption {
    pub(crate) const ALL: &[Self] = &[
        option("color", '\0', Some(COLOR_CHOICES), |options, value, _| {
            options.query.color = match value {
                "always" | "ansi" => Some(true),
                "never" => Some(false),
                "auto" => None,
                _ => return Err(COLOR_CHOICES.into()),
            };
            Ok(())
        }),
        option(
            "colors",
            '\0',
            Some("missing color"),
            |options, value, _| options.query.colors.set(value),
        ),
        option("context-separator", '\0', None, |options, value, _| {
            options.query.context_separator = Some(operands::unescape(value));
            Ok(())
        }),
        option("field-match-separator", '\0', None, |options, value, _| {
            options.query.match_separator = operands::unescape(value);
            Ok(())
        }),
        option(
            "field-context-separator",
            '\0',
            None,
            |options, value, _| {
                options.query.context_field = operands::unescape(value);
                Ok(())
            },
        ),
        option("type-add", '\0', None, |options, value, _| {
            options.edits.push(types::Edit::Add(value));
            Ok(())
        }),
        option("type-clear", '\0', None, |options, value, _| {
            options.edits.push(types::Edit::Clear(value));
            Ok(())
        }),
        option("path-separator", '\0', None, |options, value, _| {
            options.query.path_separator = operands::separator(value)?;
            Ok(())
        }),
        option("sort", '\0', None, |options, value, _| {
            options.query.scope.set_sort(value, false)
        }),
        option("sortr", '\0', None, |options, value, _| {
            options.query.scope.set_sort(value, true)
        }),
        option("pre", '\0', None, |options, value, _| {
            options.query.transform.program = (!value.is_empty()).then_some(value);
            Ok(())
        }),
        option(
            "ignore-file",
            '\0',
            Some("missing ignore file"),
            |options, value, _| {
                options.query.walk.files.push(value);
                Ok(())
            },
        ),
        option("max-filesize", '\0', None, |options, value, _| {
            options.query.scope.size = Some(scope::size(value)?);
            Ok(())
        }),
        option(
            "engine",
            '\0',
            Some("missing regex engine"),
            |options, value, _| {
                options.matcher.engine = patterns::Engine::parse(value)?;
                Ok(())
            },
        ),
        option("iglob", '\0', Some("missing glob"), |options, value, _| {
            options.glob_specs.push((value, true));
            Ok(())
        }),
        option("pre-glob", '\0', None, |options, value, _| {
            options.query.transform.add_glob(value)
        }),
        option(
            "dfa-size-limit",
            '\0',
            Some("missing size"),
            |_, value, _| scope::size(value).map(|_| ()),
        ),
        option(
            "regex-size-limit",
            '\0',
            Some("missing regex size limit"),
            |options, value, _| {
                options.matcher.regex_limit = Some(scope::size(value)?);
                Ok(())
            },
        ),
        option("hyperlink-format", '\0', None, |_, value, flag| {
            if !value.is_empty() {
                return Err(format!("unrecognized flag {flag}"));
            }
            Ok(())
        }),
        option("after-context", 'A', None, |options, value, flag| {
            options
                .context
                .set(ContextSide::After, number(value, flag)?);
            Ok(())
        }),
        option("before-context", 'B', None, |options, value, flag| {
            options
                .context
                .set(ContextSide::Before, number(value, flag)?);
            Ok(())
        }),
        option("context", 'C', None, |options, value, flag| {
            options.context.set(ContextSide::Both, number(value, flag)?);
            Ok(())
        }),
        option("max-count", 'm', None, |options, value, flag| {
            options.query.max = Some(number(value, flag)?);
            Ok(())
        }),
        option("regexp", 'e', None, |options, value, _| {
            options.patterns.push(value);
            Ok(())
        }),
        option("glob", 'g', None, |options, value, _| {
            options.glob_specs.push((value, false));
            Ok(())
        }),
        option("type", 't', None, |options, value, _| {
            options.selections.push((true, value));
            Ok(())
        }),
        option("type-not", 'T', None, |options, value, _| {
            options.selections.push((false, value));
            Ok(())
        }),
        option("encoding", 'E', None, |options, value, _| {
            options.query.encoding = encoding::Encoding::parse(value)?;
            Ok(())
        }),
        option("replace", 'r', None, |options, value, _| {
            options.replacement = Some(value);
            Ok(())
        }),
        option("max-columns", 'M', None, |options, value, _| {
            options.query.max_columns = Some(named_number(value, "max columns")?);
            Ok(())
        }),
        option("threads", 'j', None, |options, value, _| {
            options.query.threads = named_number(value, "thread count")?;
            Ok(())
        }),
        option("max-depth", 'd', None, |options, value, _| {
            options.query.scope.depth = Some(named_number(value, "depth")?);
            Ok(())
        }),
    ];
}
