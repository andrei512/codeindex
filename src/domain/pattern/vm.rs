//! Prioritized search without capture registers. Earlier threads win when paths merge.
use super::{ChunkEdges, Class, InputBoundary, Inst, Regex, Starts, Transition};
use std::ops::Range;
type Thread = (usize, usize); // instruction and leftmost byte start
#[derive(Default)]
pub(crate) struct Scratch {
    current: Vec<Thread>,
    next: Vec<Thread>,
    closure: EpsilonClosure,
}

/// A generation spans all starts that compete at the same input position.
#[derive(Default)]
pub(crate) struct EpsilonClosure {
    stack: Vec<usize>,
    marks: Marks,
}
#[derive(Default)]
pub(super) struct Marks {
    seen: Vec<u32>,
    generation: u32,
}

impl Marks {
    pub(super) fn prepare(&mut self, instructions: usize) {
        if self.seen.len() < instructions {
            self.seen.resize(instructions, 0);
        }
    }

    pub(super) fn visit(&mut self, pc: usize) -> bool {
        let mark = &mut self.seen[pc];
        if *mark == self.generation {
            return false;
        }
        *mark = self.generation;
        true
    }

    pub(super) fn advance(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.seen.fill(0);
            self.generation = 1;
        }
    }
}

impl Starts {
    pub(super) fn for_program(program: &[Inst], start: usize) -> Self {
        // A start-half assertion consumes no bytes. Its successor can still
        // rule out a starting character before entering the automaton.
        let first = match program[start] {
            Inst::Assert(super::Assertion::Word(super::Boundary::StartHalf, _), next) => next,
            _ => start,
        };
        match &program[first] {
            Inst::Assert(super::Assertion::Start(super::Anchor::Text), _) => Self::Absolute,
            Inst::Assert(super::Assertion::Start(super::Anchor::Line), _) => Self::Line,
            Inst::Consume(Class::Any, _) => Self::Anywhere,
            Inst::Consume(class, _) => Self::for_class(class),
            _ => Self::Anywhere,
        }
    }

    fn for_class(class: &Class) -> Self {
        if let Some(single) = class.singleton().filter(char::is_ascii) {
            return Self::LiteralAscii(single as u8);
        }
        Self::Ascii(std::array::from_fn(|i| class.accepts(i as u8 as char)))
    }

    /// Find the next viable byte offset. UTF-8 continuation bytes cannot hide ASCII.
    fn seek(&self, text: &str, pos: usize, prev: Option<char>) -> Option<usize> {
        let tail = &text.as_bytes()[pos..];
        match self {
            Starts::Absolute => (pos == 0).then_some(pos),
            Starts::Line => seek_line(tail, pos, prev),
            Starts::LiteralAscii(byte) => seek_literal(text, pos, *byte),
            Starts::Ascii(allowed) => tail.iter().position(|&b| !b.is_ascii() || allowed[b as usize]).map(|i| pos + i),
            _ => Some(pos),
        }
    }
}

fn seek_line(tail: &[u8], pos: usize, prev: Option<char>) -> Option<usize> {
    if pos == 0 || prev == Some('\n') {
        return Some(pos);
    }
    tail.iter().position(|&b| b == b'\n').map(|i| pos + i + 1)
}

fn seek_literal(text: &str, pos: usize, byte: u8) -> Option<usize> {
    let tail = &text.as_bytes()[pos..];
    if tail.first() == Some(&byte) {
        return Some(pos);
    }
    let width = tail.len().min(8);
    if let Some(skip) = tail[..width].iter().position(|&ch| ch == byte) {
        return Some(pos + skip);
    }
    let from = text.ceil_char_boundary(pos + width);
    text[from..].find(char::from(byte)).map(|i| from + i)
}

struct Input<'a, const BYTES: bool> {
    text: &'a str,
    pos: usize,
    prev: Option<char>,
}
impl<'a, const BYTES: bool> Input<'a, BYTES> {
    fn new(text: &'a str, pos: usize) -> Self {
        let prev = pos.checked_sub(1).and_then(|i| {
            let b = text.as_bytes()[i];
            if BYTES || b.is_ascii() { Some(char::from(b)) } else { text[..pos].chars().next_back() }
        });
        Self { text, pos, prev }
    }

    fn seek(&mut self, starts: &Starts) -> Option<()> {
        let pos = starts.seek(self.text, self.pos, self.prev)?;
        if pos != self.pos {
            *self = Self::new(self.text, pos);
        }
        Some(())
    }

    fn current(&self) -> Option<char> {
        if BYTES { self.text.as_bytes().get(self.pos).copied().map(char::from) }
        else { self.text[self.pos..].chars().next() }
    }

    fn consume(&mut self, ch: char) {
        self.pos += if BYTES { 1 } else { ch.len_utf8() };
        self.prev = Some(ch);
    }
}

impl Scratch {
    pub(super) fn new(n: usize) -> Self {
        Self {
            current: Vec::new(),
            next: Vec::new(),
            closure: EpsilonClosure {
                stack: Vec::new(),
                marks: Marks {
                    seen: vec![0; n],
                    generation: 0,
                },
            },
        }
    }
    pub(super) fn find<const BYTES: bool>(
        &mut self,
        re: &Regex,
        text: &str,
        from: usize,
    ) -> Option<Range<usize>> {
        self.find_with_edges::<BYTES>(re, text, from, ChunkEdges::default())
    }

    pub(super) fn find_with_edges<const BYTES: bool>(
        &mut self,
        re: &Regex,
        text: &str,
        from: usize,
        edges: ChunkEdges,
    ) -> Option<Range<usize>> {
        let mut input = Input::<BYTES>::new(text, from);
        let mut best = None;
        self.current.clear();
        self.next.clear();
        loop {
            // Seek only while no earlier match is still competing for priority.
            let start_new = best.is_none();
            self.seek_start(&mut input, re, start_new)?;
            let ch = input.current();
            let boundary = InputBoundary::search(input.prev, ch, edges);
            let found = self.advance(re, boundary, start_new.then_some(input.pos));
            best = found.map(|start| start..input.pos).or(best);
            let Some(current) = ch.filter(|_| best.is_none() || !self.next.is_empty()) else {
                return best;
            };
            input.consume(current);
            std::mem::swap(&mut self.current, &mut self.next);
        }
    }
    fn seek_start<const BYTES: bool>(&self, input: &mut Input<'_, BYTES>, re: &Regex, start_new: bool) -> Option<()> {
        if start_new && self.current.is_empty() {
            return input.seek(&re.starts);
        }
        Some(())
    }

    pub(super) fn prepare(&mut self, instructions: usize) {
        self.closure.marks.prepare(instructions);
    }

    /// Drain seeds in priority order. Each seed keeps its start offset while
    /// its epsilon paths share a stack of deferred fork destinations.
    fn advance(
        &mut self,
        re: &Regex,
        boundary: InputBoundary,
        start_new: Option<usize>,
    ) -> Option<usize> {
        self.closure.marks.advance();
        let seeds = self
            .current
            .drain(..)
            .chain(start_new.map(|pos| (re.start, pos)));
        for (pc, start) in seeds {
            if self
                .closure
                .expand(&re.program, (pc, start), boundary, &mut self.next)
            {
                return Some(start);
            }
        }
        None
    }
}

impl EpsilonClosure {
    /// Is there an accepting path at this boundary without consuming a scalar?
    pub(crate) fn accepts_empty(&mut self, re: &Regex, position: super::EmptyPosition) -> bool {
        match re.program[re.start] {
            Inst::Consume(..) => return false,
            Inst::Accept => return true,
            _ => {}
        }
        self.marks.prepare(re.program.len());
        self.marks.advance();
        self.walk(re.start,
            |pc| re.program[pc].transition_when(|a| position.accepts(a), |_| false),
            |_| unreachable!("an empty boundary consumes no input"))
    }

    fn expand(
        &mut self,
        program: &[Inst],
        (pc, start): Thread,
        boundary: InputBoundary,
        next_threads: &mut Vec<Thread>,
    ) -> bool {
        self.walk(
            pc,
            |pc| program[pc].transition(boundary),
            |next| next_threads.push((next, start)),
        )
    }

    fn walk(&mut self, pc: usize, step: impl Fn(usize) -> Transition, mut consume: impl FnMut(usize)) -> bool {
        let mut pending = Some(pc);
        while let Some(pc) = pending {
            if self.walk_branch(pc, &step, &mut consume) {
                self.stack.clear();
                return true;
            }
            pending = self.stack.pop();
        }
        false
    }

    fn walk_branch(&mut self, mut pc: usize, step: &impl Fn(usize) -> Transition, consume: &mut impl FnMut(usize)) -> bool {
        while self.marks.visit(pc) {
            pc = match step(pc) {
                Transition::Accept => return true,
                Transition::Save(_, next) | Transition::Follow(next) => next,
                Transition::Fork(first, alternative) => {
                    self.stack.push(alternative);
                    first
                }
                Transition::Consume(next) => {
                    consume(next);
                    break;
                }
                Transition::Dead => break,
            };
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn successive_searches_can_revisit_input_after_the_stamp_wraps() {
        let re = Regex::new("ab+c|a|b").unwrap();
        let mut scratch = Scratch::new(re.program.len());
        scratch.closure.marks.seen.fill(1);
        scratch.closure.marks.generation = u32::MAX;
        assert_eq!(scratch.find::<false>(&re, "abbbx", 0), Some(0..1));
        assert_eq!(scratch.find::<false>(&re, "abbbx", 1), Some(1..2));
    }
}
