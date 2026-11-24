//! Read unconditional SDK using directives from the nearest project and
//! Directory.Build.props/targets. Unknown imports or conditional using changes
//! leave the binding open. Other build properties are irrelevant to these facts.
use crate::domain::code::Unit;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(Default)]
struct Settings {
    sdk: bool,
    system: bool,
    aliases: HashMap<String, bool>,
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
    (settings.sdk && settings.system).then(|| {
        settings
            .aliases
            .into_iter()
            .filter_map(|(s, span)| span.then_some(s))
            .collect()
    })
}

fn attribute<'a>(mut attrs: &'a str, wanted: &str) -> Option<&'a str> {
    while !attrs.trim().is_empty() {
        attrs = attrs.trim_start();
        let end = attrs.find(|c: char| c.is_whitespace() || c == '=')?;
        let key = &attrs[..end];
        attrs = attrs[end..].trim_start().strip_prefix('=')?.trim_start();
        let quote = attrs.chars().next()?;
        if !matches!(quote, '\'' | '"') {
            return None;
        }
        attrs = &attrs[1..];
        let end = attrs.find(quote)?;
        if key == wanted {
            return Some(&attrs[..end]);
        }
        attrs = &attrs[end + 1..];
    }
    None
}

impl Settings {
    fn read(&mut self, mut xml: &str, project: bool) -> Option<()> {
        // Keep element names and conditional ancestry. '>' can occur inside a
        // quoted MSBuild attribute. Comments never contribute compiler options.
        let mut elements = Vec::new();
        while let Some((_, after)) = xml.split_once('<') {
            if let Some(rest) = after.strip_prefix("!--") {
                xml = rest.split_once("-->")?.1;
                continue;
            }
            let mut quote = None;
            let end = after.find(|ch| {
                match (quote, ch) {
                    (Some(q), c) if c == q => quote = None,
                    (None, '\'' | '"') => quote = Some(ch),
                    (None, '>') => return true,
                    _ => {}
                }
                false
            })?;
            let tag = after[..end].trim();
            xml = &after[end + 1..];
            if tag.starts_with('?') {
                continue;
            }
            if let Some(close) = tag.strip_prefix('/') {
                if elements.pop().map(|(name, _)| name) != Some(close) {
                    return None;
                }
                continue;
            }
            let empty = tag.ends_with('/');
            let mut fields = tag.trim_end_matches('/').splitn(2, char::is_whitespace);
            let tag = fields.next()?;
            let attrs = fields.next().unwrap_or("");
            let condition = elements.last().is_some_and(|&(_, c)| c)
                || attribute(attrs, "Condition").is_some()
                || matches!(tag, "Choose" | "Target");
            match tag {
                "Import" => return None,
                "Project" if project => {
                    self.sdk = attribute(attrs, "Sdk") == Some("Microsoft.NET.Sdk")
                }
                "ImplicitUsings" if !condition => {
                    self.system = matches!(xml.split_once('<')?.0.trim(), "enable" | "true")
                }
                "ImplicitUsings" => return None,
                "Using" => {
                    if condition {
                        return None;
                    }
                    if let Some(remove) = attribute(attrs, "Remove") {
                        if remove == "System" {
                            self.system = false;
                        }
                        if !remove.starts_with("System.") && remove != "System" {
                            return None;
                        }
                        if matches!(
                            remove,
                            "System.ReadOnlySpan&lt;System.Char&gt;"
                                | "System.ReadOnlySpan&lt;char&gt;"
                        ) {
                            self.aliases.retain(|_, span| !*span);
                        }
                    }
                    if let Some(include) = attribute(attrs, "Include") {
                        if let Some(alias) = attribute(attrs, "Alias") {
                            let span = matches!(
                                include,
                                "System.ReadOnlySpan&lt;System.Char&gt;"
                                    | "System.ReadOnlySpan&lt;char&gt;"
                            );
                            if self.aliases.insert(alias.into(), span).is_some() {
                                return None;
                            }
                        } else if include == "System" {
                            self.system = true;
                        }
                    } else if attribute(attrs, "Remove")
                        .is_some_and(|s| s != "System" && !s.starts_with("System."))
                        || attribute(attrs, "Update").is_some()
                    {
                        return None;
                    }
                }
                _ => {}
            }
            if !empty {
                elements.push((tag, condition));
            }
        }
        elements.is_empty().then_some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_conditions_and_removals_cannot_prove_an_alias() {
        let item = r#"<Using Alias="Text" Include="System.ReadOnlySpan&lt;char&gt;"/>"#;
        let mut settings = Settings::default();
        settings
            .read(&format!("<Project><!-- {item} --></Project>"), false)
            .unwrap();
        assert!(settings.aliases.is_empty());
        assert!(settings.read(&format!(r#"<Project><ItemGroup Condition="'$(Config)' == 'a > b'">{item}</ItemGroup></Project>"#), false).is_none());
        settings
            .read(&format!("<Project>{item}</Project>"), false)
            .unwrap();
        assert_eq!(settings.aliases.get("Text"), Some(&true));
        settings.read(r#"<Project><ItemGroup><Using Remove="System.ReadOnlySpan&lt;char&gt;"/></ItemGroup></Project>"#, false).unwrap();
        assert!(settings.aliases.is_empty());
    }
}
