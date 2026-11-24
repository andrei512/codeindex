use super::{Facts, simple, span, target};
use crate::domain::code::Analysis;
use std::collections::VecDeque;

pub(super) fn procfile(source: &str) -> Analysis {
    let mut f = Facts::default();
    for line in source.lines() {
        if let Some((name, command)) = line.split_once(':')
            && !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            && !command.trim().is_empty()
        {
            f.declare(name.into(), span(source, name), "process_type");
        }
    }
    f.finish()
}

fn batch_word(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    if line.is_empty() {
        return None;
    }
    let mut quoted = false;
    let mut escaped = false;
    for (i, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '^' if !quoted => escaped = true,
            '"' => quoted = !quoted,
            _ if !quoted && ch.is_ascii_whitespace() => return Some((&line[..i], &line[i..])),
            _ => (),
        }
    }
    (!quoted).then_some((line, ""))
}

/// Return the command after a complete, literal IF predicate. Command words
/// occurring inside operands or an echo command are not branch instructions.
fn batch_command(line: &str) -> Option<(&str, &str)> {
    let (op, rest) = batch_word(line)?;
    if !op.eq_ignore_ascii_case("if") {
        return Some((op, rest));
    }
    let (mut first, mut rest) = batch_word(rest)?;
    if first.eq_ignore_ascii_case("/i") {
        (first, rest) = batch_word(rest)?;
    }
    if first.eq_ignore_ascii_case("not") {
        (first, rest) = batch_word(rest)?;
    }
    if ["defined", "exist", "errorlevel", "cmdextversion"]
        .iter()
        .any(|kw| first.eq_ignore_ascii_case(kw))
    {
        let (_, tail) = batch_word(rest)?;
        return batch_word(tail);
    }
    let mut quoted = false;
    for (i, c) in first.char_indices() {
        if c == '"' {
            quoted = !quoted;
        }
        if !quoted && first[i..].starts_with("==") && i > 0 {
            if i + 2 < first.len() {
                return batch_word(rest);
            }
            let (_, tail) = batch_word(rest)?;
            return batch_word(tail);
        }
    }
    let (compare, tail) = batch_word(rest)?;
    if ["==", "equ", "neq", "lss", "leq", "gtr", "geq"]
        .iter()
        .any(|kw| compare.eq_ignore_ascii_case(kw))
    {
        let (_, tail) = batch_word(tail)?;
        return batch_word(tail);
    }
    None
}

// cmd.exe separators are active only outside quoting, escaping and handle redirection.
fn batch_segments(line: &str) -> impl Iterator<Item = &str> {
    let mut quoted = false;
    let mut escaped = false;
    let mut redirection = false;
    line.split(move |ch| {
        if escaped {
            escaped = false;
            redirection = false;
            return false;
        }
        let separator = !quoted && (ch == '|' || ch == '&' && !redirection);
        match ch {
            '^' if !quoted => escaped = true,
            '"' => quoted = !quoted,
            _ => {}
        }
        redirection = !quoted && matches!(ch, '>' | '<');
        separator
    })
}

pub(super) fn batch(source: &str) -> Analysis {
    let mut f = Facts::default();
    let mut owner = None;
    let mut continued = false;
    for line in source.lines() {
        let skip = continued;
        continued = line.bytes().rev().take_while(|b| *b == b'^').count() % 2 == 1;
        if skip {
            continue;
        }
        let line = line.trim().trim_start_matches('@').trim_start();
        if let Some(name) = line.strip_prefix(':') {
            if simple(name) && !name.eq_ignore_ascii_case("eof") {
                owner =
                    Some(f.declare(name.to_ascii_lowercase(), span(source, name), "batch_label"));
            }
            continue;
        }
        for command in batch_segments(line) {
            let Some((op, rest)) = batch_command(command.trim_start().trim_start_matches('@'))
            else {
                continue;
            };
            let Some((arg, _)) = batch_word(rest) else {
                continue;
            };
            let call = op.eq_ignore_ascii_case("call");
            let name = if call {
                arg.strip_prefix(':')
            } else if op.eq_ignore_ascii_case("goto") {
                Some(arg.trim_start_matches(':'))
            } else {
                None
            };
            if let Some(name) = name.filter(|s| simple(s) && !s.eq_ignore_ascii_case("eof")) {
                f.usage(
                    name.to_ascii_lowercase(),
                    span(source, name),
                    owner,
                    call,
                    false,
                );
            }
        }
    }
    f.finish()
}

pub(super) fn docker(source: &str) -> Analysis {
    let mut f = Facts::default();
    let mut owner = None;
    let mut words = Vec::new();
    let mut heredocs = VecDeque::new();
    let mut stage_count = 0usize;
    let mut escape = '\\';
    for line in source.lines() {
        if let Some(&(end, strip_tabs)) = heredocs.front() {
            let candidate = if strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if candidate == end {
                heredocs.pop_front();
            }
            continue;
        }
        let line = line.trim();
        if line.starts_with('#') {
            if line
                .trim_start_matches('#')
                .trim()
                .eq_ignore_ascii_case("escape=`")
            {
                escape = '`';
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let continued = line.strip_suffix(escape);
        words.extend(continued.unwrap_or(line).split_ascii_whitespace());
        if continued.is_some() {
            continue;
        }
        let Some((&command, args)) = words.split_first() else {
            continue;
        };
        if command.eq_ignore_ascii_case("from") {
            let mut args = args.iter().copied().skip_while(|a| a.starts_with("--"));
            owner = None;
            if let Some(base) = args.next() {
                if let (Some(op), Some(name), None) = (args.next(), args.next(), args.next())
                    && op.eq_ignore_ascii_case("as")
                    && simple(name)
                {
                    owner = Some(f.declare(
                        name.to_ascii_lowercase(),
                        span(source, name),
                        "build_stage",
                    ));
                }
                if simple(base) {
                    f.usage(
                        base.to_ascii_lowercase(),
                        span(source, base),
                        owner,
                        false,
                        true,
                    );
                }
            }
            if let Some(id) = owner {
                f.names.insert(format!("stage:{stage_count}"), vec![id]);
            }
            stage_count += 1;
        } else if command.eq_ignore_ascii_case("copy") || command.eq_ignore_ascii_case("run") {
            for &arg in args.iter().take_while(|a| a.starts_with("--")) {
                if command.eq_ignore_ascii_case("copy") {
                    if let Some(name) = arg.strip_prefix("--from=").filter(|v| simple(v)) {
                        docker_stage(&mut f, source, name, owner);
                    }
                } else if let Some(options) = arg.strip_prefix("--mount=") {
                    for opt in options.split(',') {
                        if let Some(name) = opt.strip_prefix("from=").filter(|v| simple(v)) {
                            docker_stage(&mut f, source, name, owner);
                        }
                    }
                }
            }
        }
        if ["run", "copy", "add"]
            .iter()
            .any(|c| command.eq_ignore_ascii_case(c))
        {
            for &arg in args {
                if let Some(rest) = arg
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .strip_prefix("<<")
                {
                    let name = rest.trim_start_matches('-').trim_matches(['\'', '"']);
                    if simple(name) {
                        heredocs.push_back((name, rest.starts_with('-')));
                    }
                }
            }
        }
        words.clear();
    }
    f.finish()
}

fn docker_stage(f: &mut Facts, source: &str, name: &str, owner: Option<usize>) {
    let key = match name.parse::<usize>() {
        Ok(index) => format!("stage:{index}"),
        Err(_) => name.to_ascii_lowercase(),
    };
    if !owner.is_some_and(|id| f.names.get(&key).is_some_and(|ids| ids.contains(&id))) {
        f.usage(key, span(source, name), owner, false, true);
    }
}

fn make_vars(source: &str, line: &str, owner: Option<usize>, f: &mut Facts) {
    let mut s = line;
    while let Some((_, rest)) = s.split_once('$') {
        if let Some(next) = rest.strip_prefix('$') {
            s = next;
            continue;
        }
        s = rest;
        let (inside, end) = if let Some(r) = rest.strip_prefix('(') {
            (r, ')')
        } else if let Some(r) = rest.strip_prefix('{') {
            (r, '}')
        } else {
            continue;
        };
        if let Some((name, tail)) = inside.split_once(end)
            && simple(name)
        {
            f.usage(format!("v:{name}"), span(source, name), owner, false, false);
            s = tail;
        }
    }
}

pub(super) fn make(source: &str) -> Analysis {
    let mut f = Facts::default();
    let mut owner = None;
    let mut defining = 0usize;
    #[derive(Clone, Copy)]
    enum Continuation {
        Ignore,
        Prerequisites(Option<usize>),
        Variables(Option<usize>),
    }
    let mut next_line = None;
    let mut recipe_prefix = '\t';
    for raw in source.lines() {
        let continued = raw.strip_suffix('\\');
        let raw = continued.unwrap_or(raw);
        let previous = next_line.take();
        if continued.is_some() {
            next_line = Some(Continuation::Ignore);
        }
        if let Some(previous) = previous {
            match previous {
                Continuation::Prerequisites(id) => {
                    make_prerequisites(source, raw.split('#').next().unwrap_or(""), id, &mut f)
                }
                Continuation::Variables(id) => make_vars(source, raw, id, &mut f),
                Continuation::Ignore => (),
            }
            if continued.is_some() {
                next_line = Some(previous);
            }
            continue;
        }
        if defining > 0 {
            let head = raw.split_ascii_whitespace().next();
            if head == Some("define") {
                defining += 1
            }
            if head == Some("endef") {
                defining -= 1
            }
            continue;
        }
        if let Some(recipe) = raw.strip_prefix(recipe_prefix) {
            let recipe = recipe
                .trim_start()
                .trim_start_matches(['@', '-', '+'])
                .trim_start();
            make_vars(source, recipe, owner, &mut f);
            if continued.is_some() {
                next_line = Some(Continuation::Variables(owner));
                continue;
            }
            let mut words = recipe.split_ascii_whitespace();
            if matches!(words.next(), Some("$(MAKE)" | "${MAKE}")) {
                let args: Vec<_> = words.collect();
                if args.iter().all(|s| target(s) && !s.starts_with('-')) {
                    for arg in args {
                        f.usage(format!("t:{arg}"), span(source, arg), owner, true, false);
                    }
                }
            }
            continue;
        }
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.split_ascii_whitespace().next() == Some("define") {
            defining = 1;
            owner = None;
            continue;
        }
        if let Some((lhs, rhs)) = line.split_once('=') {
            let name = lhs
                .trim_end()
                .trim_end_matches([':', '?', '+', '!'])
                .trim_end();
            if simple(name) {
                let id = f.declare(
                    format!("v:{name}"),
                    span(source, name),
                    "variable_declarator",
                );
                if name == ".RECIPEPREFIX" {
                    recipe_prefix = rhs.trim_start().chars().next().unwrap_or('\t');
                }
                make_vars(source, rhs, Some(id), &mut f);
                if continued.is_some() {
                    next_line = Some(Continuation::Variables(Some(id)));
                }
                owner = None;
                continue;
            }
        }
        owner = None;
        let Some((lhs, rhs)) = line.split_once(':') else {
            continue;
        };
        let targets: Vec<_> = lhs.split_ascii_whitespace().collect();
        if targets.is_empty() || !targets.iter().all(|s| target(s)) {
            continue;
        }
        let rhs = rhs.trim_start_matches(':').split(';').next().unwrap_or("");
        if rhs.contains('=') || rhs.contains(':') {
            continue;
        }
        for name in &targets {
            if name.starts_with('.')
                && name
                    .bytes()
                    .skip(1)
                    .all(|b| b.is_ascii_uppercase() || b == b'_')
            {
                continue;
            }
            let id = f.declare(format!("t:{name}"), span(source, name), "build_target");
            if targets.len() == 1 {
                owner = Some(id);
            }
        }
        make_prerequisites(source, rhs, owner, &mut f);
        if continued.is_some() {
            next_line = Some(Continuation::Prerequisites(owner));
        }
    }
    f.finish()
}

fn make_prerequisites(source: &str, rhs: &str, owner: Option<usize>, f: &mut Facts) {
    for dep in rhs.split_ascii_whitespace().filter(|s| target(s)) {
        f.usage(format!("t:{dep}"), span(source, dep), owner, false, false);
    }
    make_vars(source, rhs, owner, f);
}
