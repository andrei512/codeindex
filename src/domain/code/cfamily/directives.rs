use super::{
    Analyzer, Role, Token, condition, ident,
    project::{CProject, Command},
};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
enum Macro {
    Undefined,
    Number(condition::Integer),
    Alias(String),
    Defined,
    PositiveLanguageVersion,
    Unknown,
}

pub(super) struct Macros {
    names: HashMap<String, Macro>,
    configured: bool,
    native: bool,
    unsigned_char: Option<bool>,
    sdk_proofs: Vec<std::sync::Arc<super::sdk::Proof>>,
    limits: bool,
    stdint: bool,
}
impl Macros {
    fn new(command: Option<&Command>) -> Self {
        let mut m = Self {
            names: HashMap::new(),
            configured: command.is_some(),
            native: command.is_some_and(|c| c.native),
            unsigned_char: command.and_then(|c| c.unsigned_char),
            sdk_proofs: Vec::new(),
            limits: false,
            stdint: false,
        };
        if let Some(c) = command {
            m.names.insert(
                "__cplusplus".into(),
                if c.cpp {
                    c.standard.map_or(Macro::PositiveLanguageVersion, |n| {
                        Macro::Number(condition::Integer::signed(n))
                    })
                } else {
                    Macro::Undefined
                },
            );
            m.names.insert(
                "__STDC__".into(),
                Macro::Number(condition::Integer::signed(1)),
            );
            for (name, value) in &c.defines {
                let def = value
                    .as_deref()
                    .map_or(Macro::Undefined, |v| Self::replacement(Some(v)));
                m.names.insert(name.clone(), def);
            }
        }
        m
    }
    fn replacement(single: Option<&str>) -> Macro {
        match single {
            Some(v) if condition::integer(v).is_some() => {
                Macro::Number(condition::integer(v).unwrap())
            }
            Some(v) if v.starts_with(|c: char| c.is_alphabetic() || c == '_') => {
                Macro::Alias(v.to_owned())
            }
            _ => Macro::Defined,
        }
    }
    fn get(&self, name: &str) -> &Macro {
        self.names.get(name).unwrap_or_else(|| {
            if self.configured
                && !name.starts_with("__")
                && !self.sdk_proofs.iter().any(|p| p.may_modify(name))
            {
                &Macro::Undefined
            } else {
                &Macro::Unknown
            }
        })
    }
    fn standard_value(&self, name: &str) -> Option<condition::Integer> {
        if !self.native {
            return None;
        }
        (self
            .limits
            .then(|| super::standard::limits(name, self.unsigned_char))
            .flatten())
        .or_else(|| self.stdint.then(|| super::standard::stdint(name)).flatten())
    }
    pub(super) fn positive_language_version(&self, name: &str) -> bool {
        matches!(self.get(name), Macro::PositiveLanguageVersion)
    }
    pub(super) fn defined(&self, name: &str) -> Option<bool> {
        if !self.names.contains_key(name) && self.standard_value(name).is_some() {
            return Some(true);
        }
        match self.get(name) {
            Macro::Undefined => Some(false),
            Macro::Unknown => None,
            _ => Some(true),
        }
    }
    pub(super) fn number<'n>(&'n self, mut name: &'n str) -> Option<condition::Integer> {
        for _ in 0..64 {
            if !self.names.contains_key(name)
                && let Some(n) = self.standard_value(name)
            {
                return Some(n);
            }
            match self.get(name) {
                Macro::Number(n) => return Some(*n),
                Macro::Undefined => return Some(condition::Integer::signed(0)),
                Macro::Alias(to) => name = to,
                _ => return None,
            }
        }
        None
    }
}
fn both(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), v) | (v, Some(true)) => v,
        _ => None,
    }
}
struct Scan<'a, 's> {
    project: Option<(&'a CProject<'s>, &'a Command)>,
    macros: Macros,
    once: HashSet<String>,
    consulted: HashSet<String>,
    chain: Vec<String>,
    capture_owners: bool,
    owners: HashMap<usize, String>,
}
impl Scan<'_, '_> {
    fn file(
        &mut self,
        path: &str,
        t: &[Token],
        main_file: bool,
        mut active: Option<bool>,
        mut output: impl FnMut(usize, usize, Option<usize>),
    ) {
        if self.once.contains(path)
            || self.chain.len() >= 128
            || self.chain.iter().any(|p| p == path)
        {
            return;
        }
        self.chain.push(path.to_owned());
        let mut stack = Vec::new();
        let mut i = 0;
        while i < t.len() {
            let mut end = i + 1;
            while end < t.len() && t[end].line == t[i].line {
                end += 1;
            }
            if t[i].text != "#" {
                if active != Some(true) {
                    output(i, end, None);
                } else if self.capture_owners && main_file {
                    for (offset, token) in t[i..end].iter().enumerate() {
                        if let Macro::Alias(value) = self.macros.get(token.text)
                            && let Some(owner) = super::owner_macro::owner(value)
                        {
                            self.owners.insert(i + offset, owner.to_owned());
                        }
                    }
                }
                i = end;
                continue;
            }
            // Join only escaped physical newlines, never ordinary line breaks.
            let start_end = end;
            while end < t.len() && t[end - 1].text == "\\" && t[end].line == t[end - 1].line + 1 {
                let line = t[end].line;
                while end < t.len() && t[end].line == line {
                    end += 1;
                }
            }
            let command = t.get(i + 1).filter(|_| i + 1 < end).map_or("", |t| t.text);
            let args = &t[(i + 2).min(end)..end];
            let name = args.first().map_or("", |t| t.text);
            let mut definition = None;
            match command {
                "if" | "ifdef" | "ifndef" => {
                    let test = if command == "if" {
                        self.condition(args, end != start_end)
                    } else {
                        self.macros.defined(name).map(|v| v == (command == "ifdef"))
                    };
                    stack.push((active, test.map(|v| !v)));
                    active = both(active, test);
                }
                "elif" | "else" => {
                    if let Some((parent, available)) = stack.last_mut() {
                        let test = if command == "else" {
                            Some(true)
                        } else {
                            self.condition(args, end != start_end)
                        };
                        active = both(*parent, both(*available, test));
                        *available = both(*available, test.map(|v| !v));
                    }
                }
                "endif" => {
                    if let Some((parent, _)) = stack.pop() {
                        active = parent;
                    }
                }
                "define" | "undef" if active != Some(false) && args.first().is_some_and(ident) => {
                    let value = if active.is_none() {
                        Macro::Unknown
                    } else if command == "undef" {
                        Macro::Undefined
                    } else {
                        definition = Some(i + 2);
                        Macros::replacement((args.len() == 2).then(|| args[1].text))
                    };
                    self.macros.names.insert(name.to_owned(), value);
                }
                "pragma" if name == "once" && active == Some(true) => {
                    self.once.insert(path.to_owned());
                }
                "include" if active != Some(false) => {
                    if let Some((project, command)) = self.project {
                        let quoted = name.strip_prefix('"').and_then(|p| p.strip_suffix('"'));
                        let include = quoted.map(str::to_owned).or_else(|| {
                            (name == "<" && args.last().is_some_and(|t| t.text == ">"))
                                .then(|| args[1..args.len() - 1].iter().map(|t| t.text).collect())
                        });
                        if let Some(name) = include {
                            match project.include(
                                path,
                                &name,
                                quoted.is_some(),
                                command,
                                &mut self.consulted,
                            ) {
                                Ok(Some((child, tokens))) => self.header(child, &tokens, active),
                                Err(()) => self.opaque_header(),
                                Ok(None) => {
                                    let standard = super::standard::standard_header(&name);
                                    let previous_limits = self.macros.limits;
                                    let previous_stdint = self.macros.stdint;
                                    if let Some(proof) = project.sdk_proof(
                                        &name,
                                        command,
                                        active.is_none(),
                                        &mut self.consulted,
                                    ) {
                                        // Known values established after an include supersede that
                                        // include. Previously established values can be invalidated.
                                        self.macros.names.retain(|key, _| !proof.may_modify(key));
                                        if !self
                                            .macros
                                            .sdk_proofs
                                            .iter()
                                            .any(|p| std::sync::Arc::ptr_eq(p, &proof))
                                        {
                                            self.macros.sdk_proofs.push(proof);
                                        }
                                    } else {
                                        self.opaque_header();
                                    }
                                    if standard && active == Some(true) {
                                        self.macros.limits |= previous_limits;
                                        self.macros.stdint |= previous_stdint;
                                        self.macros.limits |=
                                            matches!(name.as_str(), "limits.h" | "climits");
                                        self.macros.stdint |= matches!(
                                            name.as_str(),
                                            "stdint.h" | "cstdint" | "inttypes.h" | "cinttypes"
                                        );
                                    }
                                }
                            }
                        } else {
                            self.opaque_header();
                        }
                    }
                }
                _ => {}
            }
            output(i, end, definition);
            i = end;
        }
        self.chain.pop();
    }
    fn condition(&self, args: &[Token], continued: bool) -> Option<bool> {
        if !continued {
            return condition::test(args, &self.macros);
        }
        let words = args
            .iter()
            .filter(|t| t.text != "\\")
            .map(|t| Token {
                span: t.span.clone(),
                kind: t.kind,
                text: t.text,
                line: t.line,
            })
            .collect::<Vec<_>>();
        condition::test(&words, &self.macros)
    }
    fn opaque_header(&mut self) {
        // A missing quoted header or an external directory before a repository
        // match can define or undefine application flags. Preserve uncertainty.
        self.macros.configured = false;
        self.macros.limits = false;
        self.macros.stdint = false;
        self.macros.names.clear();
    }
    fn header(&mut self, path: &str, tokens: &[Token], active: Option<bool>) {
        self.file(path, tokens, false, active, |_, _, _| {});
    }
}
impl Analyzer<'_, '_> {
    fn scan(&mut self, scan: &mut Scan, path: &str) {
        scan.file(path, self.t, true, Some(true), |i, end, definition| {
            self.skip[i..end].fill(true);
            if let Some(name) = definition {
                self.add(name, 0, "local_binding", Role::Value);
            }
        });
    }
    pub(super) fn directives(&mut self) {
        self.scan(
            &mut Scan {
                project: None,
                macros: Macros::new(None),
                once: HashSet::new(),
                consulted: HashSet::new(),
                chain: Vec::new(),
                capture_owners: false,
                owners: HashMap::new(),
            },
            "",
        );
    }
    pub(super) fn project_directives(
        &mut self,
        project: &CProject,
        command: &Command,
        path: &str,
    ) -> Vec<String> {
        let mut scan = Scan {
            project: Some((project, command)),
            macros: Macros::new(Some(command)),
            once: HashSet::new(),
            consulted: HashSet::new(),
            chain: Vec::new(),
            capture_owners: command
                .defines
                .values()
                .flatten()
                .any(|v| super::owner_macro::owner(v).is_some()),
            owners: HashMap::new(),
        };
        for (candidate, spelling) in &command.forced {
            scan.consulted.insert(candidate.clone());
            match project
                .header(candidate, command.cpp)
                .map(Some)
                .map(Ok)
                .unwrap_or_else(|| {
                    project.include("", spelling, true, command, &mut scan.consulted)
                }) {
                Ok(Some((file, tokens))) => scan.header(file, &tokens, Some(true)),
                _ => scan.opaque_header(),
            }
        }
        self.scan(&mut scan, path);
        self.macro_owners = scan.owners;
        let mut paths: Vec<_> = scan.consulted.into_iter().collect();
        paths.sort_unstable();
        paths
    }
}
