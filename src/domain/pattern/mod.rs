//! Prioritized Thompson automaton shared by content regex and byte-oriented path glob queries.
mod glob;
mod parse;
mod captures;
mod sets;
mod unicode;
pub use glob::Glob;
use std::ops::Range;
pub(crate) use unicode::letter_mark_or_number;

#[derive(Clone, Debug)]
enum Member {
    Range(char, char),
    Word(bool),
    Digit(bool),
    Space(bool),
    Set(sets::Set),
}
#[derive(Clone, Debug)]
struct Class {
    members: std::sync::Arc<[Member]>,
    negated: bool,
    fold: bool,
}
impl Class {
    fn accepts(&self, ch: char) -> bool {
        let inside = |ch| {
            self.members.iter().any(|m| match m {
                Member::Set(set) => set.contains(ch),
                Member::Range(a, b) => (*a..=*b).contains(&ch),
                Member::Word(yes) => unicode::word(ch) == *yes,
                Member::Digit(yes) => unicode::decimal(ch) == *yes,
                Member::Space(yes) => unicode::space(ch) == *yes,
            })
        };
        (inside(ch) || self.fold && unicode::folded(ch).iter().copied().any(inside)) != self.negated
    }
}
#[derive(Clone, Copy, Debug)]
enum Boundary {
    Either(bool),
    Start,
    End,
    StartHalf,
    EndHalf,
}
#[derive(Clone, Copy, Debug)]
enum Assertion {
    Start(bool),
    End(bool),
    StartCrlf,
    EndCrlf,
    Word(Boundary, bool),
}
impl Assertion {
    fn accepts(self, prev: Option<char>, next: Option<char>) -> bool {
        match self {
            Self::Start(lines) => prev.is_none() || lines && prev == Some('\n'),
            Self::End(lines) => next.is_none() || lines && next == Some('\n'),
            Self::StartCrlf => {
                prev.is_none() || prev == Some('\n') || prev == Some('\r') && next != Some('\n')
            }
            Self::EndCrlf => {
                next.is_none() || next == Some('\r') || next == Some('\n') && prev != Some('\r')
            }
            Self::Word(kind, ascii) => {
                let word = |c: char| {
                    if ascii {
                        c.is_ascii_alphanumeric() || c == '_'
                    } else {
                        unicode::word(c)
                    }
                };
                let (a, b) = (prev.is_some_and(word), next.is_some_and(word));
                match kind {
                    Boundary::Either(yes) => (a != b) == yes,
                    Boundary::Start => !a && b,
                    Boundary::End => a && !b,
                    Boundary::StartHalf => !a,
                    Boundary::EndHalf => !b,
                }
            }
        }
    }
}
#[derive(Debug)]
enum Expr {
    Capture(usize,Box<Expr>),
    Empty,
    Class(Class),
    Assert(Assertion),
    Sequence(Vec<Expr>),
    Alternatives(Vec<Expr>),
    Repeat {
        body: Box<Expr>,
        min: usize,
        max: Option<usize>,
        greedy: bool,
    },
}
// A required case-sensitive literal can discard files before NFA verification.
// Only concatenate adjacent singleton classes. Unknown constructs break runs.
fn required_literal(expr: &Expr, best: &mut String) {
    let keep = |run: &str, best: &mut String| {
        if run.len() > best.len() {
            best.clear();
            best.push_str(run);
        }
    };
    match expr {
        Expr::Sequence(items) => {
            let mut run = String::new();
            for item in items {
                if let Expr::Class(c) = item
                    && !c.negated
                    && !c.fold
                    && let [Member::Range(a, b)] = c.members.as_ref()
                    && a == b
                {
                    run.push(*a);
                } else {
                    keep(&run, best);
                    run.clear();
                    required_literal(item, best);
                }
            }
            keep(&run, best);
        }
        Expr::Repeat { body, min, .. } if *min > 0 => required_literal(body, best),
        _ => {}
    }
}
#[derive(Debug)]
enum Inst {
    Save(usize,usize),
    Accept,
    Consume(Class, usize),
    Assert(Assertion, usize),
    Fork(usize, usize),
}

#[derive(Debug)]
enum Starts {
    Anywhere,
    Absolute,
    Line,
    // Non-ASCII bytes always fall back to the Unicode VM.
    Ascii([bool; 128]),
}

#[derive(Debug)]
pub struct Regex {
    start: usize,
    program: Vec<Inst>,
    names: Vec<Option<String>>,
    required: String,
    starts: Starts,
}
pub(crate) fn word_char(c: char) -> bool { unicode::word(c) }
impl Regex {
    pub(crate) fn instruction_bytes(&self) -> usize {
        std::mem::size_of_val(self.program.as_slice())
    }
    pub fn new(pattern: &str) -> Result<Self, String> {
        Self::with_case_folding(pattern, false)
    }
    pub(crate) fn with_case_folding(pattern: &str, fold: bool) -> Result<Self, String> {
        Self::compile(parse::parse(pattern, fold)?)
    }
    pub(crate) fn folded_literal(text: &str) -> Result<Self, String> {
        if text.len() > 128 * 1024 {
            return Err("pattern too long".into());
        }
        let chars = text.chars().map(|c| Expr::Class(Class {
            members: [Member::Range(c, c)].into(),
            negated: false,
            fold: true,
        })).collect();
        Self::compile(Expr::Sequence(chars))
    }
    fn compile(expr: Expr) -> Result<Self, String> {
        let mut program = vec![Inst::Accept];
        let start = emit(&expr, 0, &mut program)?;
        let mut required = String::new();
        required_literal(&expr, &mut required);
        let starts = match &program[start] {
            Inst::Assert(Assertion::Start(false), _) => Starts::Absolute,
            Inst::Assert(Assertion::Start(true), _) => Starts::Line,
            Inst::Consume(class, _) => {
                Starts::Ascii(std::array::from_fn(|i| class.accepts(i as u8 as char)))
            }
            _ => Starts::Anywhere,
        };
        Ok(Self {
            starts,
            start,
            program,
            required,
            names: Vec::new(),
        })
    }
    pub(crate) fn byte_input(pattern:&str,fold:bool,groups:bool)->Result<Self,String> {
        let(expr,names)=parse::parse_groups(pattern,fold,groups,true)?;
        let mut re=Self::compile(expr)?;re.names=names;Ok(re)
    }
    pub(crate) fn capturing(pattern:&str,fold:bool)->Result<Self,String>{
        let (expr,names)=parse::parse_groups(pattern,fold,true,false)?;
        let mut re=Self::compile(expr)?;
        re.names=names;
        Ok(re)
    }
    pub(crate) fn capture_id(&self,name:&str)->Option<usize>{
        name.parse().ok().or_else(||self.names.iter().position(|v|v.as_deref()==Some(name)).map(|i|i+1))
    }
    pub(crate) fn groups(&self,text:&str,range:Range<usize>)->Option<Vec<Option<Range<usize>>>>{
        captures::groups(self,text,range)
    }
    pub fn required_literal(&self) -> &str {
        &self.required
    }
    pub fn matches<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        let mut scratch = Scratch::new(self.program.len());
        let mut cursor = 0;
        let mut last = None;
        std::iter::from_fn(move || {
            loop {
                if cursor > text.len() {
                    return None;
                }
                let hit = scratch.find(self, text, cursor, false)?;
                let duplicate = hit.is_empty() && last == Some(hit.end);
                cursor = if hit.is_empty() {
                    hit.end + text[hit.end..].chars().next().map_or(1, char::len_utf8)
                } else {
                    hit.end
                };
                last = Some(hit.end);
                if !duplicate {
                    return Some(hit);
                }
            }
        })
    }
}
fn push(p: &mut Vec<Inst>, inst: Inst) -> Result<usize, String> {
    if p.len() >= 65_536 {
        return Err("compiled pattern exceeds instruction limit".into());
    }
    let i = p.len();
    p.push(inst);
    Ok(i)
}
fn emit(e: &Expr, next: usize, p: &mut Vec<Inst>) -> Result<usize, String> {
    match e {
        Expr::Capture(i,body)=> { let end=push(p,Inst::Save(i*2+1,next))?; let start=emit(body,end,p)?; push(p,Inst::Save(i*2,start)) },
        Expr::Empty => Ok(next),
        Expr::Class(c) => push(p, Inst::Consume(c.clone(), next)),
        Expr::Assert(a) => push(p, Inst::Assert(*a, next)),
        Expr::Sequence(items) => items
            .iter()
            .rev()
            .try_fold(next, |n, item| emit(item, n, p)),
        Expr::Alternatives(items) => {
            let mut items = items.iter().rev();
            let mut start = emit(items.next().unwrap(), next, p)?;
            for item in items {
                let alt = emit(item, next, p)?;
                start = push(p, Inst::Fork(alt, start))?
            }
            Ok(start)
        }
        Expr::Repeat {
            body,
            min,
            max,
            greedy,
        } => {
            let fork = |a, b| {
                if *greedy {
                    Inst::Fork(a, b)
                } else {
                    Inst::Fork(b, a)
                }
            };
            let mut start = next;
            let mut required = *min;
            if let Some(max) = max {
                for _ in *min..*max {
                    let e = emit(body, start, p)?;
                    start = push(p, fork(e, start))?
                }
            } else {
                let split = push(p, Inst::Accept)?;
                let e = emit(body, split, p)?;
                p[split] = fork(e, next);
                if required == 0 {
                    // Enter through a distinct choice. When the body can match empty,
                    // reaching the loop fork must still honor its higher-priority exit.
                    start = push(p, fork(e, next))?;
                } else {
                    start = e;
                    required -= 1;
                }
            }
            for _ in 0..required {
                start = emit(body, start, p)?
            }
            Ok(start)
        }
    }
}
type Thread = (usize, usize); // instruction and leftmost byte start
struct Scratch {
    current: Vec<Thread>,
    next: Vec<Thread>,
    stack: Vec<Thread>,
    seen: Vec<usize>,
}
impl Scratch {
    fn new(n: usize) -> Self {
        Self {
            current: Vec::new(),
            next: Vec::new(),
            stack: Vec::new(),
            seen: vec![0; n],
        }
    }
    fn find(&mut self, re: &Regex, text: &str, from: usize, bytes: bool) -> Option<Range<usize>> {
        let mut prev = if bytes {
            from.checked_sub(1).map(|i| char::from(text.as_bytes()[i]))
        } else {
            text[..from].chars().next_back()
        };
        let mut pos = from;
        let mut best = None;
        self.current.clear();
        self.next.clear();
        self.seen.fill(0);
        loop {
            // Seek only while no earlier match is still competing for priority.
            if self.current.is_empty() && best.is_none() {
                let tail = &text.as_bytes()[pos..];
                match &re.starts {
                    Starts::Absolute if pos != 0 => return None,
                    Starts::Line if pos != 0 && prev != Some('\n') => {
                        pos += tail.iter().position(|&byte| byte == b'\n')? + 1;
                        prev = Some('\n');
                    }
                    Starts::Ascii(allowed) => {
                        let skip = tail
                            .iter()
                            .position(|&byte| !byte.is_ascii() || allowed[byte as usize])?;
                        if skip != 0 {
                            pos += skip;
                            prev = Some(char::from(text.as_bytes()[pos - 1]));
                        }
                    }
                    _ => {}
                }
            }
            let ch = if bytes {
                text.as_bytes().get(pos).copied().map(char::from)
            } else {
                text[pos..].chars().next()
            };
            if best.is_none() {
                self.stack.push((re.start, pos))
            }
            // Epsilon closure visits each instruction once per input offset. Earlier
            // threads win on a shared state, preserving leftmost-first alternation.
            self.stack.extend(self.current.drain(..).rev());
            while let Some((pc, start)) = self.stack.pop() {
                if self.seen[pc] == pos + 1 {
                    continue;
                }
                self.seen[pc] = pos + 1;
                match &re.program[pc] {
                    Inst::Accept => {
                        best = Some(start..pos);
                        self.stack.clear();
                        break;
                    }
                    Inst::Save(_,next) => self.stack.push((*next,start)),
                    Inst::Fork(a, b) => {
                        self.stack.push((*b, start));
                        self.stack.push((*a, start))
                    }
                    Inst::Assert(a, n) if a.accepts(prev, ch) => self.stack.push((*n, start)),
                    Inst::Consume(c, n) if ch.is_some_and(|v| c.accepts(v)) => {
                        self.next.push((*n, start))
                    }
                    _ => {}
                }
            }
            if ch.is_none() || best.is_some() && self.next.is_empty() {
                return best;
            }
            prev = ch;
            pos += if bytes { 1 } else { ch.unwrap().len_utf8() };
            std::mem::swap(&mut self.current, &mut self.next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spans(pattern: &str, input: &str) -> Vec<Range<usize>> {
        Regex::new(pattern).unwrap().matches(input).collect()
    }
    #[test]
    fn archived_queries() {
        let text = "café\nfunction() other CAFÉ";
        assert_eq!(spans("c.fé|other", text), [0..5, 17..22]);
        assert_eq!(spans("(?i)café", text), [0..5, 23..28]);
        assert_eq!(spans(r"café\nfunction", text), [0..14]);
        assert!(Regex::new("[").is_err());
        assert!(Glob::new("**/*.ts").unwrap().is_match("src/sub/a.ts"));
        assert!(Glob::new("**/*.ts").unwrap().is_match("a.ts"));
        assert!(!Glob::new("src/*.ts").unwrap().is_match("src/sub/a.ts"));
    }
    #[test]
    fn skipping_impossible_starts_preserves_offsets_and_priority() {
        assert_eq!(spans("^a*", "aaab"), [0..3]);
        assert_eq!(spans("^$", ""), [0..0]);
        assert_eq!(spans("^a|b", "xxb"), [2..3]);
        assert_eq!(spans("(?m)^ab", "é\nxab\nab"), [7..9]);
        assert_eq!(spans("[aé]+", "xxéaa"), [2..6]);
        assert_eq!(spans(r"[a]\b", "ab a!"), [3..4]);
    }
    #[test]
    fn precedence_and_boundaries() {
        assert_eq!(spans("a|aa", "aaa"), [0..1, 1..2, 2..3]);
        assert_eq!(spans("aa|a", "aaa"), [0..2, 2..3]);
        assert_eq!(spans("a*", "aba"), [0..1, 2..3]);
        assert_eq!(spans("(b?|a)*", "abaa"), [0..0, 1..4]);
        assert_eq!(spans("a+?", "aaa"), [0..1, 1..2, 2..3]);
        assert_eq!(spans("(?m)^", "a\n\n"), [0..0, 2..2, 3..3]);
        assert_eq!(spans("", "é"), [0..0, 2..2]);
        assert_eq!(spans(r"\b\w{2,3}\b", "α2"), [0..3]);
        assert_eq!(spans("(?i)ς", "Σσ"), [0..2, 2..4]);
        assert_eq!(spans("x.*z|q", "x12q3z"), [0..6]);
        assert_eq!(spans("x.*z|q", "x12q3"), [3..4]);
        assert!(Regex::new("(?=abc)").is_err());
    }
    #[test]
    fn unicode_properties_sets_and_scoped_flags() {
        assert_eq!(spans(r"[\p{Script=Greek}&&\p{Lu}]", "αΣ"), [2..4]);
        assert_eq!(spans(r"[a-e--[bc]~~[z]]+", "bczdea"), [2..6]);
        assert_eq!(spans(r"(?-u:\w+)", "éA_"), [2..4]);
        assert!(Regex::new(r"(?-u:\xFF)").is_err());
        assert_eq!(spans("(?x)(?<val> a # skip newline\n b )", "xab"), [1..3]);
        assert_eq!(spans("(?Rm)^", "a\r\nb\rc"), [0..0, 3..3, 5..5]);
        assert_eq!(spans(r"\b{start}", "α x"), [0..0, 3..3]);
        assert_eq!(spans("[--a]", "-Aa"), [0..1, 2..3]);
    }
    #[test]
    fn glob_bytes_classes_and_alternatives() {
        for s in ["a.rs", "b/a.ts", "dir/a.rs"] {
            assert!(Glob::new("**/*.{rs,ts}").unwrap().is_match(s))
        }
        assert!(!Glob::new("?.rs").unwrap().is_match("é.rs"));
        assert!(Glob::new("??.rs").unwrap().is_match("é.rs"));
        assert!(Glob::new("[!ab].rs").unwrap().is_match("c.rs"));
        assert!(Glob::new("a/{b,,c}").unwrap().is_match("a/c"));
        assert!(!Glob::new("a/{b,,c}").unwrap().is_match("a/"));
        assert!(Glob::new("[a").is_err());
    }
}
