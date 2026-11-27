pub(in crate::domain::code) mod analyzer;
use super::{Analysis, Call, Declaration, Kind, Language, Token};

/// Attribute values are source spans. Skip raw-text elements so JavaScript and
/// CSS string literals that happen to contain an ID do not create HTML symbols.
pub(super) fn html(text: &str) -> Analysis {
    analyze_html(text, None)
}

fn analyze_html(text: &str, mut tests: Option<&mut Vec<std::ops::Range<usize>>>) -> Analysis {
    let mut facts = Analysis::default();
    let mut harness = super::roles::WebHarness::default();
    html_parts(text, |part| {
        let test_script = tests.is_some() && harness.observe(&part).is_some();
        match part {
            HtmlPart::Id(span) => {
                facts.declare_at(span, "attribute");
            }
            HtmlPart::InlineScript(body) => {
                let source = &text[body.clone()];
                let analysis = inline_script(
                    source,
                    body.start,
                    tests.as_deref_mut().filter(|_| test_script),
                );
                let names = embedded_names(source, &analysis);
                embedded(&mut facts, body.start, analysis, Some(names));
            }
            HtmlPart::InlineStyle(body) => {
                let analysis = super::analyze(Language::Css, &text[body.clone()]);
                embedded(&mut facts, body.start, analysis, None);
            }
            HtmlPart::ScriptSrc(_) | HtmlPart::Comment(_) => {}
        }
    });
    facts
}
fn inline_script(
    source: &str,
    offset: usize,
    ranges: Option<&mut Vec<std::ops::Range<usize>>>,
) -> Analysis {
    let Some(ranges) = ranges else {
        return super::analyze(Language::Js, source);
    };
    let tokens = super::lexer::tokens(source, Language::Js);
    let (facts, pairs) = super::script::analyzer::javascript_from_tokens(source, &tokens);
    super::roles::harness_registrations(source, &tokens, &pairs, |first, last| {
        ranges.push(offset + first.span.start..offset + last.span.end)
    });
    facts
}
pub(super) enum HtmlPart<'a> {
    Comment(usize),
    Id(std::ops::Range<usize>),
    InlineScript(std::ops::Range<usize>),
    InlineStyle(std::ops::Range<usize>),
    ScriptSrc(&'a str),
}
fn attribute_value(bytes: &[u8], i: usize) -> (std::ops::Range<usize>, usize) {
    match bytes.get(i) {
        Some(quote @ (b'\'' | b'"')) => {
            let start = i + 1;
            let end = bytes[start..]
                .iter()
                .position(|c| c == quote)
                .map_or(bytes.len(), |n| start + n);
            (start..end, end + usize::from(end < bytes.len()))
        }
        _ => {
            let end = bytes[i..]
                .iter()
                .position(|c| c.is_ascii_whitespace() || *c == b'>')
                .map_or(bytes.len(), |n| i + n);
            (i..end, end)
        }
    }
}

fn skip_space(bytes: &[u8], at: usize) -> usize {
    bytes.len() - bytes[at..].trim_ascii_start().len()
}

fn next_attribute<'a>(
    b: &'a [u8],
    i: &mut usize,
) -> Option<(&'a [u8], Option<std::ops::Range<usize>>)> {
    // A slash or stray equals sign cannot start an attribute. Stop at the
    // closing angle even when the preceding name has no value.
    let separator = |c: &u8| c.is_ascii_whitespace() || matches!(c, b'/' | b'=');
    *i += b[*i..].iter().take_while(|c| separator(c)).count();
    let start = *i;
    match b.get(start)? {
        b'>' => {
            *i += 1;
            None
        }
        _ => {
            *i += b[start..]
                .iter()
                .take_while(|c| !separator(c) && **c != b'>')
                .count();
            let name = &b[start..*i];
            *i = skip_space(b, *i);
            let span = (b.get(*i) == Some(&b'=')).then(|| {
                let (value, next) = attribute_value(b, skip_space(b, *i + 1));
                *i = next;
                value
            });
            Some((name, span))
        }
    }
}

struct Tag<'a> {
    name: &'a str,
    position: usize,
    mime: &'a str,
    external: Option<&'a str>,
}

impl<'a> Tag<'a> {
    fn new(name: &'a str, position: usize) -> Self {
        Self {
            name,
            position,
            mime: "",
            external: None,
        }
    }

    fn observe_attribute(
        &mut self,
        text: &'a str,
        name: &[u8],
        span: Option<std::ops::Range<usize>>,
    ) -> Option<HtmlPart<'a>> {
        if name.eq_ignore_ascii_case(b"src") {
            self.external = Some(span.map_or("", |range| &text[range]));
            return None;
        }
        let value = span?;
        if name.eq_ignore_ascii_case(b"type") {
            self.mime = &text[value];
            return None;
        }
        (name.eq_ignore_ascii_case(b"id") && !value.is_empty()).then_some(HtmlPart::Id(value))
    }

    fn next_id(&mut self, text: &'a str) -> Option<HtmlPart<'a>> {
        while let Some((name, span)) = next_attribute(text.as_bytes(), &mut self.position) {
            if let Some(id) = self.observe_attribute(text, name, span) {
                return Some(id);
            }
        }
        None
    }
}

fn raw_body_end(text: &str, tag: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    text[start..]
        .match_indices("</")
        .map(|(offset, _)| start + offset)
        .find(|offset| {
            let name = offset + 2;
            bytes
                .get(name..name + tag.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(tag.as_bytes()))
                && bytes
                    .get(name + tag.len())
                    .is_some_and(|c| c.is_ascii_whitespace() || matches!(c, b'>' | b'/'))
        })
        .unwrap_or(bytes.len())
}

/// Raw elements consume everything until their own closing tag. Only supported
/// script and stylesheet media types contribute a guest region.
enum RawElement {
    Script,
    Style,
    Opaque,
}
impl RawElement {
    fn parse(tag: &str) -> Option<Self> {
        if tag.eq_ignore_ascii_case("script") {
            return Some(Self::Script);
        }
        if tag.eq_ignore_ascii_case("style") {
            return Some(Self::Style);
        }
        ["textarea", "title", "iframe"]
            .iter()
            .any(|v| tag.eq_ignore_ascii_case(v))
            .then_some(Self::Opaque)
    }

    fn part<'a>(
        &self,
        body: std::ops::Range<usize>,
        mime: &str,
        external: Option<&'a str>,
    ) -> Option<HtmlPart<'a>> {
        let mime = mime.trim();
        match self {
            Self::Script => is_script_mime(mime).then(|| external.map_or(HtmlPart::InlineScript(body), HtmlPart::ScriptSrc)),
            Self::Style => (mime.is_empty() || mime.eq_ignore_ascii_case("text/css")).then_some(HtmlPart::InlineStyle(body)),
            Self::Opaque => None,
        }
    }
}

fn is_script_mime(mime: &str) -> bool {
    [
        "",
        "module",
        "text/javascript",
        "application/javascript",
        "text/ecmascript",
        "application/ecmascript",
        "text/typescript",
    ]
    .iter()
    .any(|v| mime.eq_ignore_ascii_case(v))
}

enum Markup<'a> {
    Comment(usize),
    Tag(Tag<'a>),
}

struct HtmlParts<'a> {
    text: &'a str,
    position: usize,
    tag: Option<Tag<'a>>,
}

impl<'a> Markup<'a> {
    fn at(text: &'a str, start: usize) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes[start..].starts_with(b"!--") {
            return Some(Self::Comment(start - 1));
        }
        if !bytes.get(start).is_some_and(u8::is_ascii_alphabetic) {
            return None;
        }
        let name_byte = |c: &&u8| !c.is_ascii_whitespace() && !matches!(c, b'/' | b'>');
        let end = start + bytes[start..].iter().take_while(name_byte).count();
        Some(Self::Tag(Tag::new(&text[start..end], end)))
    }
}

impl<'a> HtmlParts<'a> {
    fn next_markup(&self) -> Option<Markup<'a>> {
        self.text[self.position..]
            .match_indices('<')
            .find_map(|(offset, _)| Markup::at(self.text, self.position + offset + 1))
    }

    fn comment(&mut self, start: usize) -> HtmlPart<'a> {
        let body = start + 4;
        self.position = self.text[body..]
            .find("-->")
            .map_or(self.text.len(), |offset| body + offset + 3);
        HtmlPart::Comment(start)
    }

    fn finish_tag(&mut self, tag: Tag<'a>) -> Option<HtmlPart<'a>> {
        self.position = tag.position;
        let raw = RawElement::parse(tag.name)?;
        let close = raw_body_end(self.text, tag.name, tag.position);
        self.position = (close + 2).min(self.text.len());
        raw.part(tag.position..close, tag.mime, tag.external)
    }

    fn markup_part(&mut self, markup: Markup<'a>) -> Option<HtmlPart<'a>> {
        let mut tag = match markup {
            Markup::Comment(start) => return Some(self.comment(start)),
            Markup::Tag(tag) => tag,
        };
        if let Some(id) = tag.next_id(self.text) {
            self.tag = Some(tag);
            return Some(id);
        }
        self.finish_tag(tag)
    }
}

impl<'a> Iterator for HtmlParts<'a> {
    type Item = HtmlPart<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let markup = self
                .tag
                .take()
                .map(Markup::Tag)
                .or_else(|| self.next_markup())?;
            if let Some(part) = self.markup_part(markup) {
                return Some(part);
            }
        }
    }
}

pub(super) fn html_parts<'a>(text: &'a str, visit: impl FnMut(HtmlPart<'a>)) {
    HtmlParts {
        text,
        position: 0,
        tag: None,
    }
    .for_each(visit);
}

// Analyze an embedded region against its own source slice and project its
// local ordinals and byte spans into the host. IDs remain HTML's public symbols.
fn embedded(
    host: &mut Analysis,
    offset: usize,
    region: Analysis,
    script_names: Option<super::ForwardNames>,
) {
    let decl_base = host.declarations.len();
    let use_base = host.calls.len();

    host.declarations
        .extend(region.declarations.into_iter().map(|d| Declaration {
            span: offset + d.span.start..offset + d.span.end,
            kind: "embedded_declaration",
        }));
    host.calls.extend(region.calls.into_iter().map(|c| Call {
        span: offset + c.span.start..offset + c.span.end,
        owner: c.owner.map(|id| decl_base + id),
        is_call: c.is_call,
    }));
    host.direct
        .extend(region.direct.into_iter().map(|(row, mut targets)| {
            for id in &mut targets {
                *id += decl_base;
            }
            (use_base + row, targets)
        }));
    if let Some(names) = script_names {
        // Persist reference evidence in source-row order across independent builds.
        for (row, targets) in names.all_rows().enumerate() {
            if !targets.is_empty() {
                host.direct.push((
                    use_base + row,
                    targets.iter().map(|t| decl_base + t.local).collect(),
                ));
            }
        }
    }
}

fn embedded_names(body: &str, region: &Analysis) -> super::ForwardNames {
    let units = [super::Unit {
        path: "embedded.ts",
        text: body,
        analysis: region,
    }];
    let mut names = super::ForwardNames::with_rows(&units);
    super::script::resolve_files(
        &[0],
        &units,
        &std::collections::HashMap::from([("embedded.ts", 0)]),
        &mut names,
    );
    names
}

// Keep the historical ID, class, pseudo ordering of selector groups.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Selector {
    Id,
    Class,
    Pseudo,
}

impl Selector {
    fn read(sigil: &str) -> Option<Self> {
        match sigil {
            "#" => Some(Self::Id),
            "." => Some(Self::Class),
            ":" => Some(Self::Pseudo),
            _ => None,
        }
    }

    fn targets(self, ids: &[usize], position: usize) -> Vec<usize> {
        match self {
            Self::Pseudo => [&ids[..position], &ids[position + 1..]].concat(),
            _ => ids.to_vec(),
        }
    }

    fn declaration(self) -> &'static str {
        match self {
            Self::Id => "id_name",
            Self::Class | Self::Pseudo => "class_name",
        }
    }
}

type SelectorGroups<'a> = std::collections::BTreeMap<(usize, Selector, &'a str), Vec<usize>>;

fn selector_names<'t, 'a>(
    tokens: &'t [Token<'a>],
) -> impl Iterator<Item = (Selector, &'t Token<'a>)> {
    let mut brackets = 0;
    tokens.windows(2).filter_map(move |pair| {
        let (sigil, name) = (&pair[0], &pair[1]);
        brackets += match sigil.text {
            "[" => 1,
            "]" => -1,
            _ => 0,
        };
        let kind = Selector::read(sigil.text)?;
        (brackets == 0 && selector_name(sigil, name)).then_some((kind, name))
    })
}

fn record_selectors<'a>(
    tokens: &[Token<'a>],
    scope: usize,
    groups: &mut SelectorGroups<'a>,
    facts: &mut Analysis,
) {
    if tokens.first().is_some_and(|t| t.text == "@") {
        return;
    }
    for (kind, name) in selector_names(tokens) {
        let id = facts.declare_at(name.span.clone(), kind.declaration());
        groups.entry((scope, kind, name.text)).or_default().push(id);
    }
}

fn selector_name(sigil: &Token, name: &Token) -> bool {
    name.kind == Kind::Word
        && name.span.start == sigil.span.end
        && !name.text.starts_with(|c: char| c.is_ascii_digit())
}

fn bind_selectors(groups: SelectorGroups<'_>, a: &mut Analysis) {
    for ((_, kind, _), ids) in groups {
        bind_selector_group(kind, &ids, a);
    }
}

fn bind_selector_group(kind: Selector, ids: &[usize], a: &mut Analysis) {
    let pseudo = kind == Selector::Pseudo;
    if pseudo && ids.len() == 1 {
        return;
    }
    for (position, &id) in ids.iter().enumerate() {
        let targets = kind.targets(ids, position);
        let d = &a.declarations[id];
        let start = d.span.start - usize::from(!pseudo);
        a.direct.push((a.calls.len(), targets));
        a.calls.push(Call {
            span: start..d.span.end,
            owner: None,
            is_call: false,
        });
    }
}

#[derive(Default)]
struct CustomProperties<'a> {
    roots: std::collections::HashSet<usize>,
    local: std::collections::HashMap<(usize, &'a str), Vec<usize>>,
    global: std::collections::HashMap<&'a str, Vec<usize>>,
    uses: Vec<(usize, &'a str, std::ops::Range<usize>)>,
}

fn property_name<'a>(tokens: &[Token<'a>]) -> Option<(&'a str, std::ops::Range<usize>)> {
    // A CSS custom property name begins with two adjacent hyphens.
    let [token, dash, name, ..] = tokens else {
        return None;
    };
    if token.text != "-"
        || dash.text != "-"
        || dash.span.start != token.span.end
        || name.kind != Kind::Word
        || name.span.start != dash.span.end
    {
        return None;
    }
    Some((name.text, token.span.start..name.span.end))
}
enum Property<'a> {
    Declaration(&'a str, std::ops::Range<usize>),
    Read(&'a str, std::ops::Range<usize>),
}

impl<'a> Property<'a> {
    fn at(t: &[Token<'a>], i: usize, in_declaration: bool) -> Option<Self> {
        let (name, span) = property_name(&t[i..])?;
        if in_declaration && t.get(i + 3).is_some_and(|token| token.text == ":") {
            return Some(Self::Declaration(name, span));
        }
        let prefix = t.get(i.checked_sub(2)?..i)?;
        (prefix[1].text == "(" && prefix[0].text.eq_ignore_ascii_case("var"))
            .then_some(Self::Read(name, span))
    }
}

impl<'a> CustomProperties<'a> {
    fn observe(&mut self, property: Property<'a>, scope: usize, a: &mut Analysis) {
        match property {
            Property::Declaration(name, span) => {
                let id = a.declare_at(span, "css_variable");
                self.local.entry((scope, name)).or_default().push(id);
                if self.roots.contains(&scope) {
                    self.global.entry(name).or_default().push(id);
                }
            }
            Property::Read(name, span) => self.uses.push((scope, name, span)),
        }
    }
    fn bind(self, a: &mut Analysis) {
        for (scope, name, span) in self.uses {
            if let Some(targets) = self
                .local
                .get(&(scope, name))
                .or_else(|| self.global.get(name))
            {
                a.direct.push((a.calls.len(), targets.clone()));
            }
            a.calls.push(Call {
                span,
                owner: None,
                is_call: false,
            });
        }
    }
}

/// The first token after a block or semicolon starts a declaration. A rule
/// prelude belongs to the containing scope until its opening brace.
#[derive(Default)]
struct CssScopes {
    blocks: Vec<usize>,
    prelude: usize,
}

enum CssPart<'t, 'a> {
    Rule(CssRule<'t, 'a>),
    Property(usize, Property<'a>),
}

impl<'a> CssPart<'_, 'a> {
    fn record(
        self,
        properties: &mut CustomProperties<'a>,
        groups: &mut SelectorGroups<'a>,
        facts: &mut Analysis,
    ) {
        match self {
            Self::Rule(rule) => rule.record(properties, groups, facts),
            Self::Property(scope, property) => properties.observe(property, scope, facts),
        }
    }
}

struct CssRule<'t, 'a> {
    parent: usize,
    body: usize,
    selector: &'t [Token<'a>],
}

impl<'a> CssRule<'_, 'a> {
    fn root(&self) -> bool {
        self.parent == 0
            && matches!(self.selector, [s, n] if s.text == ":" && n.text.eq_ignore_ascii_case("root"))
    }

    fn record(
        self,
        properties: &mut CustomProperties<'a>,
        groups: &mut SelectorGroups<'a>,
        a: &mut Analysis,
    ) {
        if self.root() {
            properties.roots.insert(self.body);
        }
        record_selectors(self.selector, self.parent, groups, a);
    }
}

impl CssScopes {
    fn current(&self) -> usize {
        self.blocks.last().copied().unwrap_or(0)
    }

    fn advance<'t, 'a>(&mut self, tokens: &'t [Token<'a>], i: usize) -> Option<CssPart<'t, 'a>> {
        let next = i + 1;
        let part = match tokens[i].text {
            "{" => {
                let parent = self.current();
                self.blocks.push(next);
                Some(CssPart::Rule(CssRule {
                    parent,
                    body: next,
                    selector: &tokens[self.prelude..i],
                }))
            }
            "}" => {
                self.blocks.pop();
                None
            }
            ";" => None,
            "-" => {
                let scope = self.current();
                return Property::at(tokens, i, i == self.prelude && scope != 0)
                    .map(|property| CssPart::Property(scope, property));
            }
            _ => return None,
        };
        self.prelude = next;
        part
    }
}

pub(super) fn css(t: &[Token<'_>]) -> Analysis {
    let mut a = Analysis::default();
    let mut scopes = CssScopes::default();
    let mut properties = CustomProperties::default();
    let mut groups = SelectorGroups::new();
    for part in (0..t.len()).filter_map(|i| scopes.advance(t, i)) {
        part.record(&mut properties, &mut groups, &mut a);
    }
    properties.bind(&mut a);
    bind_selectors(groups, &mut a);
    a
}

#[cfg(test)]
mod embedded_tests {
    use super::*;

    #[test]
    fn embedded_regions_project_local_ids_into_the_same_host() {
        let text = concat!(
            "<div id='before'></div>",
            "<script>function one() {} one();</script>",
            "<style>:root { --tone: red; } a { color: var(--tone) }</style>",
            "<script>function two() {} two();</script>",
        );
        let region = html(text);
        let edges: Vec<_> = region
            .direct
            .iter()
            .flat_map(|(row, targets)| {
                let call = &region.calls[*row].span;
                targets.iter().map(move |id| (call.start, *id))
            })
            .collect();
        for (name, spelling) in [("one", "one();"), ("--tone", "--tone)"), ("two", "two();")] {
            let def = region
                .declarations
                .iter()
                .position(|d| &text[d.span.clone()] == name)
                .unwrap();
            assert!(
                edges.contains(&(text.find(spelling).unwrap(), def)),
                "{name}"
            );
        }
    }

    #[test]
    fn embedded_script_evidence_keeps_call_order() {
        let region = html("<script>function hit() {} hit(); hit(); hit();</script>");
        assert_eq!(region.direct.len(), 3);
        assert!(region.direct.windows(2).all(|rows| rows[0].0 < rows[1].0));
        assert!(
            region
                .direct
                .iter()
                .all(|(_, targets)| targets.as_slice() == [0])
        );
    }
}
