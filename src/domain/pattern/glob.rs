use super::{Regex, Scratch};
use std::fmt::Write;

#[derive(Debug)]
pub struct Glob(Regex);
fn literal(c: char, s: &mut String) {
    for b in c.encode_utf8(&mut [0; 4]).bytes() {
        write!(s, "\\x{b:02X}").unwrap()
    }
}
impl Glob {
    pub fn new(pattern: &str) -> Result<Self, String> {
        if pattern.len() > 16_384 {
            return Err("glob is too long".into());
        }
        // Glob wildcards consume path bytes. Regex wildcards consume scalars.
        let mut out = String::from("(?s)\\A(?:");
        let mut chars = pattern.chars().peekable();
        let mut groups: Vec<(usize, usize)> = vec![];
        let mut boundary = true;
        while let Some(c) = chars.next() {
            match c {
                '*' => {
                    let mut count = 1;
                    while chars.next_if_eq(&'*').is_some() {
                        count += 1
                    }
                    if count == 2 && boundary && chars.peek() == Some(&'/') {
                        chars.next();
                        if out == "(?s)\\A(?:" && chars.peek().is_none() {
                            out.push_str(".*")
                        } else {
                            out.push_str("(?:.*/)?")
                        }
                        boundary = true;
                        continue;
                    } else if count == 2 && boundary && chars.peek().is_none() {
                        out.push_str(".*")
                    } else {
                        out.push_str("[^/]*")
                    }
                }
                '?' => out.push_str("[^/]"),
                '[' => {
                    let negated = chars.next_if(|c| *c == '!' || *c == '^').is_some();
                    out.push('[');
                    if negated {
                        out.push('^')
                    }
                    let mut first = true;
                    loop {
                        let x = chars.next().ok_or("unclosed glob character class")?;
                        if !first && x == ']' {
                            break;
                        }
                        if x == '-' && !first && chars.peek() != Some(&']') {
                            out.push('-')
                        } else {
                            literal(x, &mut out)
                        }
                        first = false;
                    }
                    out.push(']');
                }
                '{' => {
                    let start = out.len();
                    out.push_str("(?:");
                    groups.push((start, out.len()));
                }
                '}' => {
                    let (start, branch) = groups.pop().ok_or("unmatched closing brace in glob")?;
                    if branch == out.len() {
                        if branch == start + 3 {
                            out.truncate(start);
                            continue;
                        }
                        out.pop();
                    }
                    out.push(')');
                }
                ',' if !groups.is_empty() => {
                    let (_, branch) = groups.last_mut().unwrap();
                    if out.len() != *branch {
                        out.push('|');
                        *branch = out.len()
                    }
                }
                '\\' => literal(chars.next().ok_or("trailing backslash in glob")?, &mut out),
                _ => literal(c, &mut out),
            }
            boundary = matches!(c, '/' | '{') || c == ',' && !groups.is_empty();
        }
        if !groups.is_empty() {
            return Err("unclosed glob alternate".into());
        }
        out.push_str(")\\z");
        Regex::new(&out).map(Self)
    }
    pub fn is_match(&self, path: &str) -> bool {
        let mut scratch = Scratch::new(self.0.program.len());
        scratch.find(&self.0, path, 0, true).is_some()
    }
}
