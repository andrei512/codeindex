use super::{Link, Parsed, Section, inline, target};
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct Line {
    has_bracket: bool,
    start: usize,
    body: usize,
    end: usize,
    number: usize,
}
struct Block {
    lines: Vec<Line>,
    heading: Option<usize>,
}
#[derive(Default)]
struct Container {
    quotes: usize,
    list: usize,
    item_start: bool,
    indent: usize,
}

fn indent(b: &[u8]) -> usize {
    b.iter().take_while(|&&c| c == b' ').count()
}

fn content(s: &str, active_list: &mut usize) -> (usize, Container) {
    let b = s.as_bytes();
    let mut p = 0;
    let mut info = Container::default();
    loop {
        let spaces = indent(&b[p..]);
        let i = p + spaces;
        if spaces <= 3 && b.get(i) == Some(&b'>') {
            p = i + 1;
            if matches!(b.get(p), Some(b' ' | b'\t')) {
                p += 1;
            }
            info.quotes += 1;
            continue;
        }
        if spaces >= *active_list && *active_list > 0 {
            p += *active_list;
            info.list = *active_list;
            *active_list = 0;
            continue;
        }
        if spaces <= 3 {
            let digits = b[i..]
                .iter()
                .take(9)
                .take_while(|c| c.is_ascii_digit())
                .count();
            let marker = if matches!(b.get(i), Some(b'-' | b'*' | b'+')) {
                1
            } else if digits > 0 && matches!(b.get(i + digits), Some(b'.' | b')')) {
                digits + 1
            } else {
                0
            };
            if marker > 0 && matches!(b.get(i + marker), Some(b' ' | b'\t')) {
                let count = indent(&b[i + marker..]);
                let padding = if (1..=4).contains(&count) { count } else { 1 };
                p = i + marker + padding;
                info.item_start = true;
                info.list += spaces + marker + padding;
                continue;
            }
        }
        info.indent = spaces;
        break;
    }
    if info.list > 0 {
        *active_list = info.list;
    }
    (p, info)
}

fn fence(s: &str) -> Option<(u8, usize, &str)> {
    let c = *s.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = s.bytes().take_while(|&x| x == c).count();
    (n >= 3).then_some((c, n, &s[n..]))
}
fn atx(s: &str) -> Option<(usize, &str)> {
    let level = s.bytes().take_while(|&c| c == b'#').count();
    if !(1..=6).contains(&level)
        || s.as_bytes()
            .get(level)
            .is_some_and(|c| !matches!(c, b' ' | b'\t'))
    {
        return None;
    }
    Some((level, s[level..].trim()))
}
fn underline(s: &str) -> Option<usize> {
    let s = s.trim_end();
    let c = *s.as_bytes().first()?;
    if (c == b'=' || c == b'-') && s.bytes().all(|x| x == c) {
        Some(if c == b'=' { 1 } else { 2 })
    } else {
        None
    }
}
fn thematic(s: &str) -> bool {
    let mut iter = s.bytes().filter(|c| !matches!(c, b' ' | b'\t'));
    let Some(c) = iter.next() else {
        return false;
    };
    matches!(c, b'-' | b'_' | b'*') && {
        let mut n = 1;
        for x in iter {
            if x != c {
                return false;
            }
            n += 1;
        }
        n >= 3
    }
}
fn valid_title(s: &str) -> bool {
    let bytes = s.as_bytes();
    let Some(&start) = bytes.first() else {
        return false;
    };
    let close = match start {
        b'(' => b')',
        b'"' | b'\'' => start,
        _ => return false,
    };
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == close {
            return i + 1 == bytes.len();
        }
        if start == b'(' && bytes[i] == b'(' {
            return false;
        }
        if bytes[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    false
}
fn definition(s: &str) -> Option<(String, String)> {
    if !s.starts_with('[') {
        return None;
    }
    let n = s.find("]:")?;
    let label = &s[1..n];
    let mut chars = label.chars();
    let mut invalid = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if matches!(c, '[' | ']') {
            invalid = true;
            break;
        }
    }
    if label.trim().is_empty() || invalid {
        return None;
    }
    let rest = s[n + 2..].trim_start();
    if label.starts_with('^') {
        return Some((inline::key(label), String::new()));
    }
    let (url, end) = inline::destination(rest)?;
    if end == 0
        || (!rest[end..].trim().is_empty()
            && (!rest[end..].starts_with(char::is_whitespace) || !valid_title(rest[end..].trim())))
    {
        return None;
    }
    Some((inline::key(label), inline::unescape(url)))
}

fn add_section(
    parsed: &mut Parsed,
    stack: &mut Vec<usize>,
    first: Line,
    level: usize,
    end: usize,
) -> usize {
    while parsed.sections[*stack.last().unwrap()].level >= level {
        parsed.sections[stack.pop().unwrap()].end = first.start;
    }
    let id = parsed.sections.len();
    parsed.sections.push(Section {
        title: String::new(),
        anchor: String::new(),
        level,
        parent: stack.last().copied(),
        start: first.start,
        end,
        line: first.number,
    });
    stack.push(id);
    id
}
fn flush(blocks: &mut Vec<Block>, para: &mut Vec<Line>, heading: Option<usize>) {
    if heading.is_none() && !para.iter().any(|line| line.has_bracket) {
        para.clear();
    } else if !para.is_empty() {
        blocks.push(Block {
            lines: std::mem::take(para),
            heading,
        });
    }
}
fn slug(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|&c| {
            crate::domain::pattern::letter_mark_or_number(c) || matches!(c, '_' | '-' | ' ')
        })
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}
fn unique(used: &mut HashMap<String, usize>, base: String) -> String {
    let mut next = *used.entry(base.clone()).or_default();
    if next == 0 {
        used.insert(base.clone(), 1);
        return base;
    }
    loop {
        let s = format!("{base}-{next}");
        next += 1;
        if !used.contains_key(&s) {
            used.insert(base, next);
            used.insert(s.clone(), 1);
            return s;
        }
    }
}

fn attrs(s: &str) -> (&str, Option<&str>) {
    let s = s.trim_end();
    if !s.ends_with('}') {
        return (s, None);
    }
    let Some(pos) = s.rfind('{') else {
        return (s, None);
    };
    let args = &s[pos + 1..s.len() - 1];
    if args.contains(['{', '}', '<', '>', '\\', '\n', '\r']) {
        return (s, None);
    }
    let id = args
        .split_ascii_whitespace()
        .filter_map(|word| word.strip_prefix('#'))
        .filter(|s| !s.is_empty())
        .last();
    (s[..pos].trim_end(), id)
}

fn html_block(s: &str) -> Option<&'static str> {
    for (start, end) in [("<!--", "-->"), ("<?", "?>"), ("<![CDATA[", "]]>")] {
        if s.starts_with(start) {
            return Some(end);
        }
    }
    let body = s.strip_prefix('<')?.trim_start_matches('/');
    let name = body.split([' ', '\t', '/', '>']).next()?;
    let name = name.to_ascii_lowercase();
    if matches!(name.as_str(), "script" | "pre" | "style" | "textarea") {
        return Some(match name.as_str() {
            "script" => "</script>",
            "pre" => "</pre>",
            "style" => "</style>",
            _ => "</textarea>",
        });
    }
    if matches!(
        name.as_str(),
        "address"
            | "article"
            | "aside"
            | "base"
            | "basefont"
            | "blockquote"
            | "body"
            | "caption"
            | "center"
            | "col"
            | "colgroup"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "frame"
            | "frameset"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "header"
            | "hr"
            | "html"
            | "iframe"
            | "legend"
            | "li"
            | "link"
            | "main"
            | "menu"
            | "menuitem"
            | "nav"
            | "noframes"
            | "ol"
            | "p"
            | "param"
            | "search"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "title"
            | "tr"
            | "track"
            | "ul"
    ) {
        return Some("");
    }
    None
}

fn table_delimiter(line: &str) -> bool {
    let line = line.trim().trim_matches('|');
    !line.is_empty()
        && line.split('|').all(|cell| {
            let marker = cell.trim().trim_matches(':');
            !marker.is_empty() && marker.bytes().all(|b| b == b'-')
        })
}
fn table_cells(line: &str, mut scan: impl FnMut(&str, usize, &[usize])) {
    let mut start = 0;
    let mut removals = Vec::new();
    let mut cell = String::new();
    let mut copied = 0;
    for (i, _) in line
        .match_indices('|')
        .chain(std::iter::once((line.len(), "")))
    {
        if i < line.len() && i > start && line.as_bytes()[i - 1] == b'\\' {
            cell.push_str(&line[copied..i - 1]);
            removals.push(cell.len());
            copied = i;
            continue;
        }
        if removals.is_empty() {
            scan(&line[start..i], start, &[]);
        } else {
            cell.push_str(&line[copied..i]);
            scan(&cell, start, &removals);
        }
        cell.clear();
        removals.clear();
        start = i + 1;
        copied = start;
    }
}
pub(super) fn parse(path: &str, text: &str) -> Parsed {
    let root = Section {
        anchor: String::new(),
        title: path.rsplit('/').next().unwrap_or("").to_owned(),
        level: 0,
        parent: None,
        start: 0,
        end: text.len(),
        line: 1,
    };
    let mut parsed = Parsed {
        sections: vec![root],
        links: Vec::new(),
        path: String::new(),
        destinations: Vec::new(),
    };
    let mut stack = vec![0];
    let mut blocks = Vec::new();
    let mut para = Vec::new();
    let mut defs = HashMap::new();
    let mut fenced: Option<(u8, usize, usize, usize)> = None;
    let mut html = None;
    let mut start = 0;
    let mut list = 0;
    let mut last_quotes = 0;
    let mut last_list = 0;
    let mut after_definition = false;
    let mut lazy_quote = false;
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        let raw_start = start;
        start += raw.len();
        let s = raw.strip_suffix('\n').unwrap_or(raw);
        let s = s.strip_suffix('\r').unwrap_or(s);
        let (skip, container) = content(s, &mut list);
        let body = skip + container.indent;
        let line = &s[body..];
        if let Some((marker, width, quotes, depth)) = fenced {
            if quotes == container.quotes && (depth == 0 || container.list > 0 || line.is_empty()) {
                if container.indent <= 3
                    && fence(line).is_some_and(|(c, n, rest)| {
                        c == marker && n >= width && rest.trim().is_empty()
                    })
                {
                    fenced = None;
                }
                continue;
            }
            fenced = None;
        }
        if let Some(end) = html {
            if end == "" {
                if !line.is_empty() {
                    continue;
                }
                html = None;
            } else {
                if line.contains(end) {
                    html = None;
                }
                continue;
            }
        }
        if container.quotes < last_quotes && !para.is_empty() && !line.is_empty() {
            lazy_quote = true;
        }
        if line.is_empty() || container.quotes > 0 {
            lazy_quote = false;
        }
        if container.quotes != last_quotes
            || container.item_start
            || (last_list > 0 && container.list == 0 && thematic(line))
        {
            flush(&mut blocks, &mut para, None);
        }
        last_quotes = container.quotes;
        let previous_list = last_list;
        last_list = container.list;
        if after_definition && valid_title(line.trim()) {
            after_definition = false;
            continue;
        }
        after_definition = false;
        if (previous_list > 0 || para.is_empty()) && matches!(line.trim_end(), "-" | "*" | "+") {
            flush(&mut blocks, &mut para, None);
            list = 2;
            continue;
        }
        if (container.item_start && matches!(line.trim_end(), "[x]" | "[X]" | "[ ]"))
            || line.is_empty()
        {
            flush(&mut blocks, &mut para, None);
            continue;
        }
        let mut row = Line {
            has_bracket: line.as_bytes().contains(&b'['),
            start: raw_start,
            body: raw_start + body,
            end: raw_start + s.len(),
            number: i + 1,
        };
        if container.indent <= 3 {
            if let Some((marker, n, rest)) = fence(line) {
                if marker != b'`' || !rest.contains('`') {
                    flush(&mut blocks, &mut para, None);
                    fenced = Some((marker, n, container.quotes, container.list));
                    continue;
                }
            }
            if let Some(end) = html_block(line) {
                flush(&mut blocks, &mut para, None);
                if end.is_empty() || !line.contains(end) {
                    html = Some(end);
                }
                continue;
            }
            if let Some((level, title)) = atx(line) {
                flush(&mut blocks, &mut para, None);
                let id = add_section(&mut parsed, &mut stack, row, level, text.len());
                row.body = raw_start + body + (title.as_ptr() as usize - line.as_ptr() as usize);
                row.end = row.body + title.len();
                blocks.push(Block {
                    lines: vec![row],
                    heading: Some(id),
                });
                continue;
            }
            if !para.is_empty() && !lazy_quote {
                if let Some(level) = underline(line) {
                    let id = add_section(&mut parsed, &mut stack, para[0], level, text.len());
                    flush(&mut blocks, &mut para, Some(id));
                    continue;
                }
            }
            if thematic(line) {
                flush(&mut blocks, &mut para, None);
                continue;
            }
            if para.is_empty() {
                if let Some((key, url)) = definition(line) {
                    defs.entry(key).or_insert(url);
                    after_definition = true;
                    continue;
                }
            }
        } else if para.is_empty() {
            continue;
        }
        para.push(row);
    }
    flush(&mut blocks, &mut para, None);
    for block in &mut blocks {
        if block.heading.is_some() {
            continue;
        }
        let mut skip = 0;
        while let Some(first) = block.lines.get(skip) {
            if !text[first.body..first.end].starts_with('[') {
                break;
            }
            let mut input = String::from(&text[first.body..first.end]);
            let mut found = None;
            for (n, next) in block.lines[skip + 1..].iter().enumerate() {
                if input.len() > 4096 {
                    break;
                }
                input.push('\n');
                input.push_str(&text[next.body..next.end]);
                if let Some(pair) = definition(&input) {
                    found = Some((pair, n + 2));
                    break;
                }
            }
            let Some(((key, url), n)) = found else {
                break;
            };
            defs.entry(key).or_insert(url);
            skip += n;
        }
        if skip > 0 {
            block.lines.drain(..skip);
        }
    }
    let mut used = HashMap::from([(String::new(), 1)]);
    for block in blocks {
        let Some(&first) = block.lines.first() else {
            continue;
        };
        let single = block.lines.len() == 1;
        if block.heading.is_none() && !block.lines.iter().any(|row| row.has_bracket) {
            continue;
        }
        if block.heading.is_none()
            && block
                .lines
                .get(1)
                .is_some_and(|s| table_delimiter(&text[s.body..s.end]))
            && text[first.body..first.end].contains('|')
        {
            let section = parsed.section_at(first.body);
            for (n, row) in block.lines.iter().enumerate() {
                if n == 1 {
                    continue;
                }
                table_cells(&text[row.body..row.end], |cell, start, removed| {
                    inline::scan(cell, &defs, false, |pos, url| {
                        if let Some((target, anchor)) = target::local_address(url) {
                            let offset =
                                row.body + start + pos + removed.partition_point(|&r| r <= pos);
                            parsed.destinations.push(Link {
                                target,
                                anchor,
                                offset,
                                line: row.number,
                                section,
                            });
                        }
                    });
                });
            }
            continue;
        }
        let mut joined = String::new();
        let mut offsets = Vec::new();
        let raw = if single {
            &text[first.body..first.end]
        } else {
            for row in &block.lines {
                if !offsets.is_empty() {
                    joined.push('\n');
                }
                offsets.push(joined.len());
                joined.push_str(&text[row.body..row.end]);
            }
            &joined
        };
        let (mut source, id) = if block.heading.is_some() {
            attrs(raw)
        } else {
            (raw, None)
        };
        if block.heading.is_some() && text[first.start..first.body].contains('#') {
            let stripped = source.trim_end_matches('#');
            if stripped.is_empty() || stripped.ends_with([' ', '\t']) {
                source = stripped.trim_end();
            }
        }
        let section = block
            .heading
            .unwrap_or_else(|| parsed.section_at(first.body));
        let title = inline::scan(source, &defs, block.heading.is_some(), |pos, url| {
            if let Some((target, anchor)) = target::local_address(url) {
                let idx = offsets
                    .partition_point(|&start| start <= pos)
                    .saturating_sub(1);
                let offset = block.lines[idx].body + pos - offsets.get(idx).copied().unwrap_or(0);
                parsed.destinations.push(Link {
                    target,
                    anchor,
                    offset,
                    line: block.lines[idx].number,
                    section,
                });
            }
        });
        if let Some(h) = block.heading {
            let section = &mut parsed.sections[h];
            section.title = title.trim().to_owned();
            section.anchor = unique(
                &mut used,
                id.filter(|s| !s.is_empty())
                    .map_or_else(|| slug(&section.title), str::to_owned),
            );
        }
    }
    parsed.relocate(path);
    parsed
}
