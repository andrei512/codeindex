//! Recover capture positions for the winning span only when a replacement
//! requests them. Ordinary searches carry no per-thread capture registers.
use super::{Regex,Inst};
use std::ops::Range;
pub(super) fn groups(re:&Regex,text:&str,span:Range<usize>)->Option<Vec<Option<Range<usize>>>> {
    let mut stack=vec![(re.start,vec![None;(re.names.len()+1)*2])];
    let mut seen=vec![usize::MAX;re.program.len()];
    let mut next=Vec::new();
    let mut pos=span.start;
    let mut prev=text[..pos].chars().next_back();
    loop {
        let ch=text[pos..].chars().next();
        while let Some((pc,mut slots))=stack.pop() {
            if seen[pc]==pos {continue}
            seen[pc]=pos;
            match &re.program[pc] {
                Inst::Save(slot,dest)=>{slots[*slot]=Some(pos);stack.push((*dest,slots));}
                Inst::Fork(first,second)=>{stack.push((*second,slots.clone()));stack.push((*first,slots));}
                Inst::Assert(a,dest) if a.accepts(prev,ch)=>stack.push((*dest,slots)),
                Inst::Consume(c,dest) if pos<span.end && ch.is_some_and(|c1|c.accepts(c1))=>next.push((*dest,slots)),
                Inst::Accept if pos==span.end=>{
                    slots[0]=Some(span.start);slots[1]=Some(span.end);
                    return Some(slots.chunks_exact(2).map(|s|Some(s[0]?..s[1]?)).collect());
                }
                _=>{}
            }
        }
        if next.is_empty() {return None}
        prev=ch;
        pos+=ch?.len_utf8();
        stack.extend(next.drain(..).rev());
    }
}
