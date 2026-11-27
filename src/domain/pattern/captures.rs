//! Replay the winning span for replacements. Forks share registers until
//! a path writes a new capture position.
use super::{ChunkEdges, InputBoundary, Inst, Regex, Transition, vm::Marks};
use std::{ops::Range, rc::Rc};

const UNSET: usize = usize::MAX;

#[derive(Default)]
pub(crate) struct CaptureScratch {
    single: Threads<[usize; 2]>,
    multiple: Threads<Rc<[usize]>>,
    marks: Marks,
}

#[derive(Default)]
struct Threads<R> {
    stack: Vec<(usize, R)>,
    next: Vec<(usize, R)>,
}

pub(crate) struct CaptureSpan<'a> {
    pub text: &'a str,
    pub span: Range<usize>,
    pub edges: ChunkEdges,
}

impl CaptureScratch {
    pub(crate) fn groups(
        &mut self,
        re: &Regex,
        matched: CaptureSpan,
    ) -> Option<Vec<Option<Range<usize>>>> {
        self.marks.prepare(re.program.len());
        if re.names.len() <= 1 {
            self.single
                .replay(re, matched, [UNSET; 2], &mut self.marks)
        } else {
            let registers: Rc<[usize]> = std::iter::repeat_n(UNSET, re.names.len() * 2).collect();
            self.multiple
                .replay(re, matched, registers, &mut self.marks)
        }
    }
}

trait Registers: AsRef<[usize]> + Clone {
    fn save(&mut self, slot: usize, at: usize);
}

impl Registers for [usize; 2] {
    fn save(&mut self, slot: usize, at: usize) {
        self[slot] = at;
    }
}

impl Registers for Rc<[usize]> {
    fn save(&mut self, slot: usize, at: usize) {
        Rc::make_mut(self)[slot] = at;
    }
}

impl<R: Registers> Threads<R> {
    fn replay(
        &mut self,
        re: &Regex,
        matched: CaptureSpan,
        registers: R,
        marks: &mut Marks,
    ) -> Option<Vec<Option<Range<usize>>>> {
        let CaptureSpan { text, span, edges } = matched;
        self.stack.push((re.start, registers));
        let mut pos = span.start;
        let mut prev = text[..pos].chars().next_back();
        loop {
            let ch = text[pos..].chars().next();
            let input = CapturePosition::new(pos, &span, edges, (prev, ch));
            if let Some(slots) = self.step(&re.program, input, marks) {
                return Some(captured_groups(span, slots.as_ref(), re.names.len()));
            }
            prev = ch;
            pos += self.resume(ch)?.len_utf8();
        }
    }

    fn resume(&mut self, ch: Option<char>) -> Option<char> {
        let ch = ch.filter(|_| !self.next.is_empty())?;
        std::mem::swap(&mut self.stack, &mut self.next);
        self.stack.reverse();
        Some(ch)
    }

    fn step(
        &mut self,
        program: &[Inst],
        input: CapturePosition,
        marks: &mut Marks,
    ) -> Option<R> {
        marks.advance();
        while let Some((pc, slots)) = self.stack.pop() {
            if let Some(accepted) = self.visit(program, input, marks, (pc, slots)) {
                return Some(accepted);
            }
        }
        None
    }

    fn visit(&mut self, program: &[Inst], input: CapturePosition, marks: &mut Marks, (pc, slots): (usize, R)) -> Option<R> {
        if !marks.visit(pc) {
            return None;
        }
        self.transition(program[pc].transition(input.boundary), input, slots)
    }

    fn transition(&mut self, transition: Transition, input: CapturePosition, mut slots: R) -> Option<R> {
        match transition {
            Transition::Save(slot, dest) => {
                slots.save(slot - 2, input.pos);
                self.stack.push((dest, slots));
            }
            Transition::Fork(first, second) => {
                self.stack.push((second, slots.clone()));
                self.stack.push((first, slots));
            }
            Transition::Follow(dest) => self.stack.push((dest, slots)),
            Transition::Consume(dest) => self.next.push((dest, slots)),
            Transition::Accept if input.pos == input.end => {
                self.stack.clear();
                self.next.clear();
                return Some(slots);
            }
            _ => {}
        }
        None
    }
}

#[derive(Clone, Copy)]
struct CapturePosition {
    boundary: InputBoundary,
    pos: usize,
    end: usize,
}

impl CapturePosition {
    fn new(pos: usize, span: &Range<usize>, edges: ChunkEdges, (prev, ch): (Option<char>, Option<char>)) -> Self {
        let edges = ChunkEdges {
            left_invalid: edges.left_invalid && pos == span.start,
            right_invalid: edges.right_invalid && pos == span.end,
        };
        Self { pos, boundary: InputBoundary::replay(prev, ch, edges, pos < span.end), end: span.end }
    }
}

fn captured_groups(span: Range<usize>, slots: &[usize], count: usize) -> Vec<Option<Range<usize>>> {
    let captures = slots.chunks_exact(2).take(count).map(|pair| {
        (pair[0] != UNSET && pair[1] != UNSET).then(|| pair[0]..pair[1])
    });
    std::iter::once(Some(span)).chain(captures).collect()
}
