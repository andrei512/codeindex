use super::super::{Facts, UseMode, simple, span};
use crate::domain::code::Analysis;
use std::collections::VecDeque;

#[derive(Clone, Copy)]
enum DockerCommand {
    From,
    Copy,
    Run,
    Add,
    Other,
}

impl DockerCommand {
    fn parse(word: &str) -> Self {
        if word.eq_ignore_ascii_case("from") {
            Self::From
        } else if word.eq_ignore_ascii_case("copy") {
            Self::Copy
        } else if word.eq_ignore_ascii_case("run") {
            Self::Run
        } else if word.eq_ignore_ascii_case("add") {
            Self::Add
        } else {
            Self::Other
        }
    }
}

#[derive(Default)]
struct Heredocs<'a>(VecDeque<(&'a str, bool)>);

impl<'a> Heredocs<'a> {
    fn consumes(&mut self, line: &str) -> bool {
        let Some(&(end, strip_tabs)) = self.0.front() else {
            return false;
        };
        let candidate = if strip_tabs {
            line.trim_start_matches('\t')
        } else {
            line
        };
        if candidate == end {
            self.0.pop_front();
        }
        true
    }

    fn queue(&mut self, command: DockerCommand, args: &[&'a str]) {
        if !matches!(
            command,
            DockerCommand::Run | DockerCommand::Copy | DockerCommand::Add
        ) {
            return;
        }
        self.0.extend(args.iter().filter_map(|arg| {
            let rest = arg
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .strip_prefix("<<")?;
            let name = rest.trim_start_matches('-').trim_matches(['\'', '"']);
            simple(name).then_some((name, rest.starts_with('-')))
        }));
    }
}

fn docker_instructions<'a>(
    source: &'a str,
    mut comment: impl FnMut(usize),
    mut instruction: impl FnMut(DockerCommand, &[&'a str]),
) {
    let mut parser = DockerInstructions {
        source,
        words: Vec::new(),
        heredocs: Heredocs::default(),
        escape: '\\',
    };
    for raw in source.lines() {
        parser.read(raw, &mut comment, &mut instruction);
    }
}

struct DockerInstructions<'a> {
    source: &'a str,
    words: Vec<&'a str>,
    heredocs: Heredocs<'a>,
    escape: char,
}

impl<'a> DockerInstructions<'a> {
    fn read(
        &mut self,
        raw: &'a str,
        comment: &mut impl FnMut(usize),
        instruction: &mut impl FnMut(DockerCommand, &[&'a str]),
    ) {
        if self.heredocs.consumes(raw) {
            return;
        }
        let line = raw.trim();
        if line.starts_with('#') {
            comment(span(self.source, line).start);
            self.escape = directive_escape(line).unwrap_or(self.escape);
            return;
        }
        if line.is_empty() {
            return;
        }
        let continued = line.strip_suffix(self.escape);
        self.words
            .extend(continued.unwrap_or(line).split_ascii_whitespace());
        if continued.is_none() {
            complete_instruction(&mut self.words, &mut self.heredocs, instruction);
        }
    }
}

fn directive_escape(line: &str) -> Option<char> {
    line.trim_start_matches('#')
        .trim()
        .eq_ignore_ascii_case("escape=`")
        .then_some('`')
}

fn complete_instruction<'a>(
    words: &mut Vec<&'a str>,
    heredocs: &mut Heredocs<'a>,
    instruction: &mut impl FnMut(DockerCommand, &[&'a str]),
) {
    let Some((&word, args)) = words.split_first() else {
        return;
    };
    let command = DockerCommand::parse(word);
    instruction(command, args);
    heredocs.queue(command, args);
    words.clear();
}

pub(in crate::domain::code) fn docker_comments(source: &str, comment: impl FnMut(usize)) {
    docker_instructions(source, comment, |_, _| {});
}

#[derive(Default)]
struct DockerStages {
    facts: Facts,
    owner: Option<usize>,
    count: usize,
}

impl DockerStages {
    fn from(&mut self, source: &str, args: &[&str]) {
        let options = args.iter().take_while(|a| a.starts_with("--")).count();
        let operands = &args[options..];
        self.owner = match operands {
            [_, keyword, name] if keyword.eq_ignore_ascii_case("as") && simple(name) => {
                let id = self.facts.declare(
                    name.to_ascii_lowercase(),
                    span(source, name),
                    "build_stage",
                );
                self.facts
                    .names
                    .insert(format!("stage:{}", self.count), Some(id));
                Some(id)
            }
            _ => None,
        };
        self.count += 1;
        if let Some(&base) = operands.first().filter(|s| simple(s)) {
            self.facts.usage(
                base.to_ascii_lowercase(),
                span(source, base),
                self.owner,
                UseMode::EarlierReference,
            );
        }
    }

    fn reference(&mut self, source: &str, name: &str) {
        if !simple(name) {
            return;
        }
        let key = match name.parse::<usize>() {
            Ok(index) => format!("stage:{index}"),
            Err(_) => name.to_ascii_lowercase(),
        };
        if !self
            .owner
            .is_some_and(|id| self.facts.unique(&key) == Some(id))
        {
            self.facts.usage(
                key,
                span(source, name),
                self.owner,
                UseMode::EarlierReference,
            );
        }
    }

    fn instruction(&mut self, source: &str, command: DockerCommand, args: &[&str]) {
        match command {
            DockerCommand::From => self.from(source, args),
            DockerCommand::Copy => self.copy_references(source, args),
            DockerCommand::Run => self.mount_references(source, args),
            _ => {}
        }
    }

    fn copy_references(&mut self, source: &str, args: &[&str]) {
        let options = args.iter().take_while(|arg| arg.starts_with("--"));
        for name in options.filter_map(|arg| arg.strip_prefix("--from=")) {
            self.reference(source, name);
        }
    }

    fn mount_references(&mut self, source: &str, args: &[&str]) {
        let options = args.iter().take_while(|arg| arg.starts_with("--"));
        let mounts = options.filter_map(|arg| arg.strip_prefix("--mount="));
        for name in mounts
            .flat_map(|mount| mount.split(','))
            .filter_map(|arg| arg.strip_prefix("from="))
        {
            self.reference(source, name);
        }
    }
}

pub(in crate::domain::code) fn docker(source: &str) -> Analysis {
    let mut stages = DockerStages::default();
    docker_instructions(
        source,
        |_| {},
        |command, args| stages.instruction(source, command, args),
    );
    stages.facts.finish()
}
