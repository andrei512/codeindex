//! Read unconditional SDK using directives from the nearest project and
//! Directory.Build.props/targets. Unknown imports or conditional using changes
//! leave the binding open. Other build properties are irrelevant to these facts.
use crate::domain::code::Unit;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    path::Path,
};

pub(super) struct Projects<'a, 's> {
    units: &'a [Unit<'s>],
    paths: &'a HashMap<&'s str, usize>,
    projects: HashMap<&'s Path, Option<usize>>,
    owners: HashMap<&'s Path, Option<&'s Path>>,
    configs: HashMap<usize, Option<HashSet<String>>>,
}
impl<'a, 's> Projects<'a, 's> {
    pub(super) fn new(units: &'a [Unit<'s>], paths: &'a HashMap<&'s str, usize>) -> Self {
        let mut projects = HashMap::new();
        for (id, unit) in units
            .iter()
            .enumerate()
            .filter(|(_, u)| u.path.ends_with(".csproj"))
        {
            if let Some(dir) = Path::new(unit.path).parent() {
                crate::domain::code::record_unique(&mut projects, dir, id);
            }
        }
        Self {
            units,
            paths,
            projects,
            owners: HashMap::new(),
            configs: HashMap::new(),
        }
    }

    pub(super) fn for_source(&mut self, path: &'s str) -> Option<&HashSet<String>> {
        let dir = Path::new(path).parent()?;
        let owner = crate::domain::code::nearest_owner(&mut self.owners, dir, |dir| {
            self.projects.get_key_value(dir).map(|(&parent, _)| parent)
        })?;
        let project = self.projects[owner]?;
        self.configs
            .entry(project)
            .or_insert_with(|| config(project, self.units, self.paths))
            .as_ref()
    }
}

#[derive(Default)]
struct Settings<'s> {
    sdk: bool,
    system: bool,
    aliases: Vec<(Cow<'s, str>, Cow<'s, str>)>,
}

pub(super) fn config(
    project: usize,
    units: &[Unit<'_>],
    paths: &HashMap<&str, usize>,
) -> Option<HashSet<String>> {
    let dir = Path::new(units[project].path).parent()?;
    let nearest = |name| {
        dir.ancestors()
            .find_map(|p| paths.get(p.join(name).to_str()?).copied())
    };
    let mut settings = Settings::default();
    for file in [
        nearest("Directory.Build.props"),
        Some(project),
        nearest("Directory.Build.targets"),
    ]
    .into_iter()
    .flatten()
    {
        settings.read(units[file].text, file == project)?;
    }
    settings.span_aliases()
}

#[derive(Clone, Copy)]
#[repr(usize)]
enum Attribute {
    Condition,
    Sdk,
    Remove,
    Include,
    Update,
    Alias,
}

impl Attribute {
    fn read(spelling: &str) -> Option<Self> {
        const RESERVED: &[(&str, Attribute)] = &[
            ("Condition", Attribute::Condition),
            ("Sdk", Attribute::Sdk),
            ("Remove", Attribute::Remove),
            ("Include", Attribute::Include),
            ("Update", Attribute::Update),
        ];
        // Item metadata uses case-insensitive last assignment.
        if spelling.eq_ignore_ascii_case("Alias") {
            return Some(Self::Alias);
        }
        RESERVED.iter().find_map(|(name, field)| (*name == spelling).then_some(*field))
    }
}

#[derive(Default)]
struct Attributes<'a> {
    values: [Option<&'a str>; 6],
}
impl<'a> Attributes<'a> {
    fn read(mut rest: &'a str) -> Option<Self> {
        let mut fields = Self::default();
        let mut names = HashSet::new();
        rest = rest.trim_start();
        while !rest.is_empty() {
            let (key, value, after) = next_attribute(rest)?;
            rest = after.trim_start();
            if !names.insert(key) {
                return None;
            }
            fields.record(key, value);
        }
        Some(fields)
    }

    fn record(&mut self, key: &str, value: &'a str) {
        if let Some(field) = Attribute::read(key) {
            self.values[field as usize] = Some(value);
        }
    }

    fn get(&self, field: Attribute) -> Option<&'a str> {
        self.values[field as usize]
    }
}

fn next_attribute(rest: &str) -> Option<(&str, &str, &str)> {
    let rest = rest.trim_start();
    let end = rest.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = &rest[..end];
    let first = *key.as_bytes().first()?;
    if !matches!(first, b'A'..=b'Z' | b'a'..=b'z' | b'_' | b':' | 0x80..=0xff) {
        return None;
    }
    let rest = rest[end..].trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let (value, after) = rest[1..].split_once(quote)?;
    if !after.is_empty() && !after.starts_with(char::is_whitespace) {
        return None;
    }
    Some((key, value, after))
}

fn span_type(target: &str) -> bool {
    matches!(
        target,
        "System.ReadOnlySpan<System.Char>" | "System.ReadOnlySpan<char>"
    )
}

// MSBuild treats semicolons as item separators after the XML layer has
// decoded character references. Plain type names remain borrowed.
fn item_value(raw: &str) -> Option<Cow<'_, str>> {
    let Some((prefix, mut rest)) = raw.split_once('&') else {
        return Some(raw.into());
    };
    let mut text = String::from(prefix);
    loop {
        let (entity, tail) = rest.split_once(';')?;
        text.push(entity_scalar(entity)?);
        let Some((part, next)) = tail.split_once('&') else {
            text.push_str(tail);
            return Some(text.into());
        };
        text.push_str(part);
        rest = next;
    }
}

fn entity_scalar(entity: &str) -> Option<char> {
    match entity {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => xml_scalar(entity),
    }
}

fn xml_scalar(entity: &str) -> Option<char> {
    let number = entity.strip_prefix('#')?;
    let code = match number.strip_prefix('x') {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => number.parse().ok()?,
    };
    char::from_u32(code)
}

/// Consume the next XML element and leave the caller at its text content.
fn next_tag<'a>(xml: &mut &'a str) -> Result<Option<&'a str>, ()> {
    while let Some((_, after)) = xml.split_once('<') {
        let (tag, tail) = read_markup(after)?;
        *xml = tail;
        if tag.is_some() {
            return Ok(tag);
        }
    }
    Ok(None)
}

fn read_markup(after: &str) -> Result<(Option<&str>, &str), ()> {
    if let Some(comment) = after.strip_prefix("!--") {
        return Ok((None, comment.split_once("-->").ok_or(())?.1));
    }
    let end = tag_end(after)?;
    let tag = after[..end].trim();
    Ok(((!tag.starts_with('?')).then_some(tag), &after[end + 1..]))
}

fn tag_end(after: &str) -> Result<usize, ()> {
    let mut delimiters = after.match_indices(['\'', '"', '>']);
    while let Some((at, delimiter)) = delimiters.next() {
        if delimiter == ">" {
            return Ok(at);
        }
        delimiters
            .find(|(_, close)| *close == delimiter)
            .ok_or(())?;
    }
    Err(())
}

impl<'s> Settings<'s> {
    fn span_aliases(self) -> Option<HashSet<String>> {
        if !self.sdk || !self.system {
            return None;
        }
        // Conflicts are meaningful only after all ordered item removals have run.
        let mut seen = HashSet::new();
        for (alias, _) in &self.aliases {
            if !seen.insert(alias.as_ref()) {
                return None;
            }
        }
        Some(self.aliases.into_iter()
            .filter(|(_, target)| span_type(target))
            .map(|(alias, _)| alias.into_owned())
            .collect())
    }

    fn remove(&mut self, spec: &str) -> Option<()> {
        let spec = item_value(spec)?;
        // Wildcards and evaluated expressions require MSBuild. Validate the
        // complete list before changing either the implicit or aliased imports.
        if spec.contains(['*', '?', '$', '@', '%']) {
            return None;
        }
        let mut names = spec.split(';').map(str::trim).filter(|s| !s.is_empty());
        let Some(first) = names.next() else {
            return Some(());
        };
        if let Some(second) = names.next() {
            // Borrow item spellings. Neither lookup nor reordering copies the aliases.
            let mut targets: Vec<_> = [first, second].into_iter().chain(names).collect();
            let compare = |a: &str, b: &str| {
                a.bytes()
                    .map(|b| b.to_ascii_lowercase())
                    .cmp(b.bytes().map(|b| b.to_ascii_lowercase()))
            };
            targets.sort_unstable_by(|a, b| compare(a, b));
            self.remove_targets(|v| targets.binary_search_by(|t| compare(t, v)).is_ok());
        } else {
            self.remove_targets(|v| v.eq_ignore_ascii_case(first));
        }
        Some(())
    }

    fn remove_targets(&mut self, contains: impl Fn(&str) -> bool) {
        self.system &= !contains("System");
        self.aliases.retain(|(_, included)| !contains(included));
    }

    fn using(&mut self, attrs: &Attributes<'s>) -> Option<()> {
        if let Some(target) = attrs.get(Attribute::Remove) {
            self.remove(target)?;
        }
        let Some(raw) = attrs.get(Attribute::Include) else {
            return attrs.get(Attribute::Update).is_none().then_some(());
        };
        self.include(raw, attrs.get(Attribute::Alias))
    }

    fn include(&mut self, raw: &'s str, alias: Option<&'s str>) -> Option<()> {
        let target = item_value(raw)?;
        match alias {
            Some(alias) => self.aliases.push((item_value(alias)?, target)),
            None if target == "System" => self.system = true,
            None => {}
        }
        Some(())
    }

    fn read(&mut self, xml: &'s str, project: bool) -> Option<()> {
        visit_elements(xml, |element| self.observe(element, project))
    }

    fn observe(&mut self, element: ProjectElement<'s>, project: bool) -> Option<()> {
        match element.name {
            "Import" => None,
            "Project" if project => {
                self.sdk = element.dotnet_sdk();
                Some(())
            }
            "Using" | "ImplicitUsings" => self.observe_using(element),
            _ => Some(()),
        }
    }

    fn observe_using(&mut self, element: ProjectElement<'s>) -> Option<()> {
        if element.conditional {
            return None;
        }
        if element.name == "Using" {
            return self.using(&element.attrs);
        }
        self.system = element.enabled()?;
        Some(())
    }
}

struct ProjectElement<'a> {
    name: &'a str,
    attrs: Attributes<'a>,
    conditional: bool,
    tail: &'a str,
}
impl ProjectElement<'_> {
    fn dotnet_sdk(&self) -> bool {
        self.attrs.get(Attribute::Sdk).and_then(item_value).as_deref() == Some("Microsoft.NET.Sdk")
    }
    fn enabled(&self) -> Option<bool> {
        let value = item_value(self.content()?)?;
        let value = value.trim();
        Some(value.eq_ignore_ascii_case("enable") || value.eq_ignore_ascii_case("true"))
    }
    fn content(&self) -> Option<&str> {
        self.tail.split_once('<').map(|(text, _)| text)
    }
}

/// Visit opening elements in source order. Closing elements restore the
/// enclosing condition, including through tags irrelevant to using resolution.
fn visit_elements<'s>(
    mut xml: &'s str,
    mut visit: impl FnMut(ProjectElement<'s>) -> Option<()>,
) -> Option<()> {
    let mut scopes = Vec::new();
    while let Some(tag) = next_tag(&mut xml).ok()? {
        let inherited = scopes.last().is_some_and(|&(_, conditional)| conditional);
        match ElementTag::read(tag, inherited, xml)? {
            ElementTag::Close(name) => {
                if scopes.pop().map(|(open, _)| open) != Some(name) {
                    return None;
                }
            }
            ElementTag::Open { element, empty } => {
                if !empty {
                    scopes.push((element.name, element.conditional));
                }
                visit(element)?;
            }
        }
    }
    scopes.is_empty().then_some(())
}

enum ElementTag<'a> {
    Close(&'a str),
    Open { element: ProjectElement<'a>, empty: bool },
}

impl<'a> ElementTag<'a> {
    fn read(tag: &'a str, inherited: bool, tail: &'a str) -> Option<Self> {
        if let Some(name) = tag.strip_prefix('/') {
            return Some(Self::Close(name));
        }
        let empty = tag.strip_suffix('/');
        let mut fields = empty.unwrap_or(tag).splitn(2, char::is_whitespace);
        let name = fields.next()?;
        let attrs = Attributes::read(fields.next().unwrap_or(""))?;
        let conditional = inherited || attrs.get(Attribute::Condition).is_some()
            || matches!(name, "Choose" | "Target");
        Some(Self::Open {
            element: ProjectElement { name, attrs, conditional, tail },
            empty: empty.is_some(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_character_references_and_boolean_property_case_follow_msbuild() {
        let mut settings = Settings::default();
        settings.read(r#"<Project Sdk="Microsoft.NET.Sd&#107;"><PropertyGroup><ImplicitUsings>TrUe</ImplicitUsings></PropertyGroup></Project>"#,true).unwrap();
        assert!(settings.sdk && settings.system);
        settings.read("<Project><PropertyGroup><ImplicitUsings>false</ImplicitUsings></PropertyGroup></Project>",false).unwrap();
        assert!(!settings.system);
    }
    #[test]
    fn explicit_system_using_decodes_the_item_identity_before_binding() {
        let mut settings = Settings::default();
        settings.read(r#"<Project Sdk="Microsoft.NET.Sdk"><ItemGroup><Using Include="Syst&#x65;m"/><Using Include="System.ReadOnlySpan&lt;char&gt;" Alias="Text"/></ItemGroup></Project>"#,true).unwrap();
        assert!(settings.span_aliases().unwrap().contains("Text"));
    }

    #[test]
    fn removal_uses_the_item_identity_before_checking_alias_conflicts() {
        let mut settings = Settings {
            sdk: true,
            system: true,
            ..Settings::default()
        };
        settings
            .read(
                r#"<Project><Using Include="System.Int32" Alias="Text"/>
            <Using Include="System.ReadOnlySpan&lt;char&gt;" Alias="Text"/>
            <Using Remove="System.Int32"/></Project>"#,
                false,
            )
            .unwrap();
        assert_eq!(
            settings.span_aliases(),
            Some(HashSet::from(["Text".to_owned()]))
        );
    }

    #[test]
    fn attributes_reject_invalid_xml_and_observe_native_metadata_order() {
        for invalid in [
            r#"Alias="A" Alias="B""#,
            r#"Alias="A" malformed"#,
            r#"Alias="A"Other="B""#,
            r#"Other="1" Other="2""#,
        ] {
            assert!(Attributes::read(invalid).is_none());
        }
        let attrs = Attributes::read(r#"Alias="First" Include='Target' ALIAS="Second""#).unwrap();
        assert_eq!(attrs.get(Attribute::Alias), Some("Second"));
        assert_eq!(attrs.get(Attribute::Include), Some("Target"));
    }

    #[test]
    fn comments_conditions_and_removals_cannot_prove_an_alias() {
        let item = r#"<Using Alias="Text" Include="System.ReadOnlySpan&lt;char&gt;"/>"#;
        let comment = format!("<Project><!-- {item} --></Project>");
        let conditional = format!(
            r#"<Project><ItemGroup Condition="'$(Config)' == 'a > b'">{item}</ItemGroup></Project>"#
        );
        let direct = format!("<Project>{item}</Project>");
        let mut settings = Settings::default();
        settings.read(&comment, false).unwrap();
        assert!(settings.aliases.is_empty());
        assert!(settings.read(&conditional, false).is_none());
        settings.read(&direct, false).unwrap();
        assert_eq!(
            settings.aliases,
            [(
                Cow::Borrowed("Text"),
                Cow::Borrowed("System.ReadOnlySpan<char>")
            )]
        );
        settings.read(r#"<Project><ItemGroup><Using Remove="System.ReadOnlySpan&lt;char&gt;"/></ItemGroup></Project>"#, false).unwrap();
        assert!(settings.aliases.is_empty());
    }
}
