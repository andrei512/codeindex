//! Only prune syntax when captured configuration proves a target predicate
//! false. Features, custom cfg values and an unrecognized target stay unknown.
use super::*;
use std::ops::Range;

pub(super) enum Predicate {
    Atom(String, String),
    All(Vec<Predicate>),
    Any(Vec<Predicate>),
    Not(Box<Predicate>),
    Unknown,
}
pub(super) struct Condition {
    span: Range<usize>,
    predicate: Predicate,
}
#[derive(Clone, Copy)]
struct Target {
    os: &'static str,
    arch: &'static str,
    family: &'static str,
}
impl Target {
    fn host() -> Self {
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            family: std::env::consts::FAMILY,
        }
    }
    fn triple(v: &str) -> Option<Self> {
        let triple: Vec<_> = v.split('-').collect();
        let os = match triple.as_slice() {
            [_, "apple", "darwin"] => "macos",
            [_, "linux", "android" | "androideabi"]
            | [_, _, "linux", "android" | "androideabi"] => "android",
            [_, _, "linux", _] => "linux",
            [_, _, "windows", _] => "windows",
            [_, _, "freebsd"] => "freebsd",
            _ => return None,
        };
        let arch = match v.split('-').next()? {
            "aarch64" => "aarch64",
            "x86_64" => "x86_64",
            "i686" | "i586" => "x86",
            _ => "",
        };
        Some(Self {
            os,
            arch,
            family: if os == "windows" { "windows" } else { "unix" },
        })
    }
    fn config(s: &str) -> Option<Option<Self>> {
        let mut build = false;
        for l in s.lines() {
            let l = l.split('#').next()?.trim();
            if l.starts_with('[') {
                build = l == "[build]";
                continue;
            }
            let Some((key, val)) = l.split_once('=') else {
                continue;
            };
            if build && key.trim() == "target" {
                let text = val.trim().trim_matches(&['\'', '"'][..]);
                return Some(Self::triple(text));
            }
        }
        None
    }
}
impl Predicate {
    fn eval(&self, target: Option<Target>) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::Atom(k, v) => match k.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                "unix" | "windows" => Some(target?.family == k),
                "target_family" => Some(target?.family == v),
                "target_os" => Some(target?.os == v),
                "target_arch" if !target?.arch.is_empty() => Some(target?.arch == v),
                _ => None,
            },
            Self::Not(x) => Some(!x.eval(target)?),
            Self::All(xs) | Self::Any(xs) => {
                let all = matches!(self, Self::All(_));
                let mut known = true;
                for x in xs {
                    match x.eval(target) {
                        Some(v) if v != all => return Some(v),
                        None => known = false,
                        _ => {}
                    }
                }
                known.then_some(all)
            }
        }
    }
}
fn parse(t: &[Token], i: &mut usize, end: usize, depth: usize) -> Predicate {
    if *i >= end || depth > 32 {
        *i = end;
        return Predicate::Unknown;
    }
    let head = word(t, *i);
    *i += 1;
    if word(t, *i) == "=" {
        *i += 1;
        let value = word(t, *i);
        *i += 1;
        return value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .map_or(Predicate::Unknown, |s| {
                Predicate::Atom(head.into(), s.into())
            });
    }
    if word(t, *i) != "(" {
        return Predicate::Atom(head.into(), String::new());
    }
    *i += 1;
    let mut args = Vec::new();
    while *i < end && word(t, *i) != ")" {
        args.push(parse(t, i, end, depth + 1));
        match word(t, *i) {
            "," => *i += 1,
            ")" => break,
            _ => {
                *i = end;
                return Predicate::Unknown;
            }
        }
    }
    if *i >= end {
        return Predicate::Unknown;
    }
    *i += 1;
    match head {
        "all" => Predicate::All(args),
        "any" => Predicate::Any(args),
        "not" if args.len() == 1 => Predicate::Not(Box::new(args.pop().unwrap())),
        _ => Predicate::Unknown,
    }
}
pub(super) fn condition(t: &[Token], pairs: &[usize], hash: usize) -> Option<Condition> {
    if word(t, hash + 2) != "cfg" || word(t, hash + 3) != "(" {
        return None;
    }
    let close = *pairs.get(hash + 3)?;
    if close == usize::MAX {
        return None;
    }
    let mut i = hash + 4;
    let predicate = parse(t, &mut i, close, 0);
    if i != close {
        return None;
    }
    let mut item = pairs[hash + 1].checked_add(1)?;
    while word(t, item) == "#" && word(t, item + 1) == "[" {
        item = pairs[item + 1].checked_add(1)?;
    }
    let start = t.get(item)?.span.start;
    let delimiter = until(t, pairs, item, &["{", ";", ",", "}"]);
    let finish = if word(t, delimiter) == "{" && pairs[delimiter] != usize::MAX {
        pairs[delimiter]
    } else {
        delimiter.min(t.len() - 1)
    };
    Some(Condition {
        span: start..t[finish].span.end,
        predicate,
    })
}
pub(super) fn disabled(units: &[Unit]) -> Vec<Vec<Range<usize>>> {
    let mut configs = HashMap::new();
    for unit in units {
        let p = Path::new(unit.path);
        let Some(dir) = p
            .parent()
            .filter(|d| d.file_name().is_some_and(|n| n == ".cargo"))
        else {
            continue;
        };
        let priority = match p.file_name().and_then(|s| s.to_str()) {
            Some("config") => 2,
            Some("config.toml") => 1,
            _ => continue,
        };
        let Some(base) = dir.parent() else {
            continue;
        };
        if configs.get(base).is_none_or(|(old, _)| *old < priority) {
            configs.insert(base, (priority, Target::config(unit.text)));
        }
    }
    units
        .iter()
        .map(|u| {
            let target = Path::new(u.path)
                .ancestors()
                .find_map(|p| configs.get(p).and_then(|(_, t)| *t))
                .unwrap_or(Some(Target::host()));
            let mut ranges: Vec<Range<usize>> = Vec::new();
            if let Some(e) = &u.analysis.rust {
                for c in &e.conditions {
                    if c.predicate.eval(target) == Some(false) {
                        if let Some(last) =
                            ranges.last_mut().filter(|last| last.end >= c.span.start)
                        {
                            last.end = last.end.max(c.span.end);
                        } else {
                            ranges.push(c.span.clone());
                        }
                    }
                }
            }
            ranges
        })
        .collect()
}
pub(super) fn active(ranges: &[Range<usize>], at: usize) -> bool {
    !ranges
        .get(ranges.partition_point(|r| r.end <= at))
        .is_some_and(|r| r.start <= at)
}

crate::domain::code::cache::record!(Condition { span, predicate });

impl crate::domain::code::cache::Wire for Predicate{fn save(&self,w:&mut impl crate::domain::code::cache::Output){match self{Self::Atom(a, b)=>{ w.push(0); a.save(w);b.save(w);},Self::All(a)=>{ w.push(1); a.save(w);},Self::Any(a)=>{ w.push(2); a.save(w);},Self::Not(a)=>{ w.push(3); a.save(w);},Self::Unknown=>{ w.push(4); }}}fn load(r:&mut &[u8])->std::io::Result<Self>{use crate::domain::code::cache::Wire;match usize::load(r)?{0=>Ok(Self::Atom(Wire::load(r)?, Wire::load(r)?)),1=>Ok(Self::All(Wire::load(r)?)),2=>Ok(Self::Any(Wire::load(r)?)),3=>Ok(Self::Not(Wire::load(r)?)),4=>Ok(Self::Unknown),_=>Err(crate::domain::code::cache::bad())}}}
