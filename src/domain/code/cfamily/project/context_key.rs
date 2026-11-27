//! Stable identifiers encode effective settings in canonical map-key order.
use super::Command;
use std::{collections::BTreeMap, fmt::Write};
fn string(s: &str) -> String {
    let mut out = format!("s{}/", s.len());
    for byte in s.bytes() {
        write!(out, "{byte:02x}").unwrap();
    }
    out
}
fn list(items: impl IntoIterator<Item = String>) -> String {
    let mut items = items.into_iter();
    let mut body = String::new();
    let mut n = 0;
    for item in &mut items {
        body.push_str(&item);
        n += 1;
    }
    format!("l{n}/{body}")
}
fn map(values: BTreeMap<&str, String>) -> String {
    let mut out = format!("m{}/", values.len());
    for (key, value) in values {
        out += &string(key);
        out += &value;
    }
    out
}
fn boolean(b: bool) -> String {
    if b { "t" } else { "f" }.into()
}
impl Command {
    pub(in crate::domain::code::cfamily) fn context_key(&self, path: &str) -> String {
        let mut opaque: Vec<_> = self
            .search
            .all()
            .filter(|r| r.location == super::Location::Opaque)
            .map(|r| string(&r.path))
            .collect();
        opaque.sort_unstable();
        opaque.dedup();
        let settings = map(BTreeMap::from([
            (
                "defines",
                map(self
                    .defines
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_deref().map_or_else(|| "n".into(), string)))
                    .collect()),
            ),
            (
                "forced",
                list(
                    self.forced
                        .iter()
                        .map(|(p, s)| list([string(p), string(s)])),
                ),
            ),
            (
                "includes",
                list(self.search.includes.iter().map(|r| string(&r.path))),
            ),
            ("native", boolean(self.native)),
            ("opaque_dirs", list(opaque)),
            (
                "quote_dirs",
                list(self.search.quote.iter().map(|r| string(&r.path))),
            ),
            (
                "standard",
                self.standard
                    .map_or_else(|| "n".into(), |i| format!("i{i}/")),
            ),
            (
                "unsigned_char",
                self.unsigned_char.map_or_else(|| "n".into(), boolean),
            ),
        ]));
        format!(
            "ctx1/{}",
            list([
                string(if self.cpp { "cpp" } else { "c" }),
                string(path),
                string(""),
                settings
            ])
        )
    }
}
