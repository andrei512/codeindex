use super::{Facts, UseMode, simple, span, target};
use crate::domain::code::Analysis;
use crate::domain::code::lexer::bracket_literal as bracket;

#[derive(Clone, Copy, PartialEq)]
enum Form {
    Word,
    Quoted,
    Bracket,
    Open,
    Close,
}
#[derive(Clone, Copy)]
pub(in crate::domain::code) struct Arg<'a> {
    pub(in crate::domain::code) text: &'a str,
    form: Form,
}

impl<'a> Arg<'a> {
    fn literal(&self) -> Option<&'a str> {
        matches!(self.form, Form::Word | Form::Quoted).then_some(self.text)
    }
}
fn tokens(mut s: &str) -> Vec<Arg<'_>> {
    let mut out = Vec::new();
    while let Some(&first) = {
        s = s.trim_start_matches(char::is_whitespace);
        s.as_bytes().first()
    } {
        let (tail, argument) = item(s, first);
        s = tail;
        out.extend(argument);
    }
    out
}

fn item(s: &str, first: u8) -> (&str, Option<Arg<'_>>) {
    if first == b'#' {
        return (after_comment(s), None);
    }
    let (argument, width) = read_argument(s, first);
    (&s[width..], Some(argument))
}

fn read_argument(s: &str, first: u8) -> (Arg<'_>, usize) {
    if let Some((_, end)) = bracket(s) {
        return (
            Arg {
                text: &s[..end],
                form: Form::Bracket,
            },
            end,
        );
    }
    let form = match first {
        b'(' => Form::Open,
        b')' => Form::Close,
        _ => return argument(s, first == b'"'),
    };
    (
        Arg {
            text: &s[..1],
            form,
        },
        1,
    )
}

fn after_comment(s: &str) -> &str {
    if let Some((_, end)) = bracket(&s[1..]) {
        return &s[1 + end..];
    }
    s.split_once('\n').map_or("", |(_, rest)| rest)
}

fn argument(s: &str, quoted: bool) -> (Arg<'_>, usize) {
    let start = usize::from(quoted);
    let end = super::super::unescaped_chars(&s[start..])
        .find(|&(_, ch)| argument_end(ch, quoted))
        .map_or(s.len(), |(i, _)| start + i);
    let consumed = end + usize::from(quoted && end < s.len());

    (
        Arg {
            text: &s[start..end],
            form: if quoted { Form::Quoted } else { Form::Word },
        },
        consumed,
    )
}

fn argument_end(ch: char, quoted: bool) -> bool {
    if quoted {
        ch == '"'
    } else {
        ch.is_whitespace() || matches!(ch, '(' | ')' | '#')
    }
}

fn next_command<'t, 's>(
    remaining: &mut &'t [Arg<'s>],
) -> Option<(&'s str, &'t [Arg<'s>], &'s str)> {
    let start = remaining
        .windows(2)
        .position(|p| p[0].form == Form::Word && p[1].form == Form::Open)?;
    let name = remaining[start].text;
    let args = &remaining[start + 2..];
    let mut depth = 1usize;
    let end = args.iter().position(|arg| {
        match arg.form {
            Form::Open => depth += 1,
            Form::Close => depth -= 1,
            _ => (),
        }
        depth == 0
    })?;
    *remaining = &args[end + 1..];
    Some((name, &args[..end], args[end].text))
}

/// Navigation and CTest consume the same borrowed CMake command and argument spans.
pub(in crate::domain::code) struct Commands<'a>(Vec<Arg<'a>>);
impl<'a> Commands<'a> {
    pub(in crate::domain::code) fn read(source: &'a str) -> Self {
        Self(tokens(source))
    }
    pub(in crate::domain::code) fn iter(
        &self,
    ) -> impl Iterator<Item = (&'a str, &[Arg<'a>], &'a str)> {
        let mut remaining = &self.0[..];
        std::iter::from_fn(move || next_command(&mut remaining))
    }
}

fn callable(
    f: &mut Facts<String>,
    source: &str,
    args: &[Arg],
    name: Option<&str>,
) -> Option<usize> {
    let own = name.filter(|s| simple(s)).map(|s| {
        f.declare(
            format!("f:{}", s.to_ascii_lowercase()),
            span(source, s),
            "function_definition",
        )
    });

    for arg in args
        .iter()
        .skip(1)
        .filter(|a| a.form == Form::Word && simple(a.text))
    {
        f.declare(
            format!("v:{own:?}:{}", arg.text),
            span(source, arg.text),
            "parameter",
        );
    }
    own
}

pub(in crate::domain::code) fn analyze(source: &str) -> Analysis {
    let mut scope = Scope {
        source,
        facts: Facts::default(),
        owners: Vec::new(),
    };
    for (name, args, _) in Commands::read(source).iter() {
        scope.command(name, args);
    }
    scope.facts.finish()
}

struct Scope<'a> {
    source: &'a str,
    facts: Facts,
    owners: Vec<Option<usize>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Callable,
    End,
    Build { alias: bool },
    Set,
    Dependencies,
    Properties,
    Target,
    Other,
}

const TARGET_OPERATIONS: &[&str] = &[
    "target_sources",
    "target_compile_definitions",
    "target_compile_features",
    "target_compile_options",
    "target_include_directories",
    "target_link_directories",
    "target_link_options",
    "target_precompile_headers",
];

impl Command {
    fn read(op: &str) -> Self {
        match op {
            "function" | "macro" => Self::Callable,
            "endfunction" | "endmacro" => Self::End,
            "add_executable" | "add_library" => Self::Build { alias: true },
            "add_custom_target" => Self::Build { alias: false },
            "set" | "option" => Self::Set,
            "target_link_libraries" | "add_dependencies" => Self::Dependencies,
            _ => Self::target_operation(op),
        }
    }

    fn target_operation(op: &str) -> Self {
        if op == "set_target_properties" {
            return Self::Properties;
        }
        if TARGET_OPERATIONS.contains(&op) {
            return Self::Target;
        }
        Self::Other
    }
}

impl Scope<'_> {
    fn command(&mut self, cmd: &str, args: &[Arg]) {
        let op = cmd.to_ascii_lowercase();
        let first = args.first().and_then(Arg::literal);
        let owner = self.owners.last().copied().flatten();
        let command = Command::read(&op);
        let (arg_owner, variable) = match command {
            Command::Callable => {
                self.owners
                    .push(callable(&mut self.facts, self.source, args, first));
                return;
            }
            Command::End => {
                self.owners.pop();
                return;
            }
            Command::Build { .. } => (self.build_target(first).or(owner), None),
            Command::Set => (owner, assignment_name(first, args)),
            _ => {
                self.facts.usage(
                    format!("f:{op}"),
                    span(self.source, cmd),
                    owner,
                    UseMode::EarlierCall,
                );
                (owner, None)
            }
        };
        self.argument_uses(
            command,
            args,
            Owners {
                lexical: owner,
                argument: arg_owner,
            },
        );
        if let Some(name) = variable {
            self.facts.declare(
                format!("v:{owner:?}:{name}"),
                span(self.source, name),
                "variable_declarator",
            );
        }
    }

    fn build_target(&mut self, name: Option<&str>) -> Option<usize> {
        let name = name.filter(|name| target(name))?;
        Some(
            self.facts
                .declare(format!("t:{name}"), span(self.source, name), "build_target"),
        )
    }

    fn argument_uses(&mut self, command: Command, args: &[Arg], owners: Owners) {
        let (targets, dependency_owner) = target_arguments(command, args, &self.facts);
        for (j, s) in args
            .iter()
            .enumerate()
            .filter_map(|(i, a)| Some((i, a.literal()?)))
        {
            if targets.contains(&j)
                && target(s)
                && !matches!(
                    s,
                    "PRIVATE" | "PUBLIC" | "INTERFACE" | "debug" | "optimized" | "general"
                )
            {
                let owner = if j == 0 {
                    owners.argument
                } else {
                    dependency_owner.or(owners.argument)
                };
                self.facts.usage(
                    format!("t:{s}"),
                    span(self.source, s),
                    owner,
                    UseMode::Reference,
                );
            }
            self.variable_uses(s, owners);
        }
    }
    fn variable_uses(&mut self, text: &str, owners: Owners) {
        for name in variable_names(text) {
            let local = format!("v:{:?}:{name}", owners.lexical);
            // Only a value supplied in this lexical body proves a local binding.
            if self.facts.names.contains_key(&local) {
                self.facts.usage(
                    local,
                    span(self.source, name),
                    owners.argument,
                    UseMode::EarlierReference,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Owners {
    lexical: Option<usize>,
    argument: Option<usize>,
}

fn assignment_name<'a>(first: Option<&'a str>, args: &[Arg]) -> Option<&'a str> {
    if args.len() <= 1 || args.last().is_some_and(|arg| arg.text == "PARENT_SCOPE") {
        return None;
    }
    first.filter(|name| simple(name))
}

fn variable_names(s: &str) -> impl Iterator<Item = &str> {
    let mut close = 0;
    s.match_indices("${")
        .filter(|(open, _)| s[..*open].bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 0)
        .map_while(move |(open, _)| {
            let start = open + 2;
            if close < start {
                close = start + s[start..].find('}')?;
            }
            Some(&s[start..close])
        })
        .filter(|name| simple(name))
}

/// CMake commands designate targets by argument position. The remaining
/// arguments may still contain variable expansions.
fn target_arguments(
    command: Command,
    args: &[Arg<'_>],
    facts: &Facts,
) -> (std::ops::Range<usize>, Option<usize>) {
    let positions = match command {
        Command::Dependencies => {
            let owner = args
                .first()
                .and_then(Arg::literal)
                .and_then(|name| facts.unique(&format!("t:{name}")));
            return (0..args.len(), owner);
        }
        Command::Build { alias: true } if args.get(1).is_some_and(|a| a.text == "ALIAS") => 2..3,
        Command::Properties => {
            0..args
                .iter()
                .position(|a| a.text == "PROPERTIES")
                .unwrap_or(args.len())
        }
        Command::Target => 0..1,
        _ => 0..0,
    };
    (positions, None)
}
