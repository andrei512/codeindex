//! Project-local evidence for written script source directives and protobuf imports.
use super::{Analysis, Call, Declaration, Kind, Token};
use std::collections::HashMap;
mod proto;
mod shell;
pub(super) enum Evidence {
    Proto(proto::Evidence),
    Shell(shell::Evidence),
}
pub(super) fn analyze_proto(t: &[Token]) -> Analysis {
    proto::analyze(t)
}
pub(super) fn analyze_shell(text: &str) -> Analysis {
    shell::analyze(text)
}
pub(super) fn resolve(
    units: &[super::Unit],
    paths: &HashMap<&str, usize>,
    names: &mut super::Names,
) {
    proto::resolve(units, paths, names);
    shell::resolve(units, paths, names);
}

#[cfg(test)]
mod tests {
    use crate::domain::code::{self, Analysis, Language, Names, Site, Unit};
    fn bindings(lang: Language, files: &[(&str, &str)]) -> (Vec<Analysis>, Names) {
        let a = files
            .iter()
            .map(|(_, t)| code::analyze(lang, t))
            .collect::<Vec<_>>();
        let units = files
            .iter()
            .zip(&a)
            .map(|(&(path, text), analysis)| Unit {
                path,
                text,
                analysis,
            })
            .collect::<Vec<_>>();
        let n = Names::build(&units);
        (a, n)
    }
    #[test]
    fn proto_imports_nested_names_and_public_visibility() {
        let files = [
            ("project/defs.proto", "package peer; message Item {}"),
            ("project/public.proto", "import public \"defs.proto\";"),
            (
                "project/nested/main.proto",
                "package own; import \"public.proto\"; message Item {} message Outer { message Item {} Item inner = 1; .peer.Item other = 2; }",
            ),
            ("unrelated/defs.proto", "package peer; message Item {}"),
            ("project/private.proto", "import \"defs.proto\";"),
            (
                "project/test.proto",
                "import \"private.proto\"; message No { peer.Item x = 1; }",
            ),
        ];
        let (a, n) = bindings(Language::Proto, &files);
        let src = files[2].1;
        for (anchor, dest_file, target) in [
            ("Item inner", 2, src.find("Item {} Item inner").unwrap()),
            (".peer.Item other", 0, files[0].1.find("Item").unwrap()),
        ] {
            let byte = src.find(anchor).unwrap();
            let use_id = a[2]
                .calls
                .iter()
                .position(|c| c.span.start == byte)
                .unwrap();
            let targets = &n.targets[&Site {
                file: 2,
                local: use_id,
            }];
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].file, dest_file);
            assert_eq!(
                a[dest_file].declarations[targets[0].local].span.start,
                target
            );
        }
        assert!(!n.targets.keys().any(|s| s.file == 5));
    }
    #[test]
    fn shell_source_order_and_dynamic_commands() {
        let files = [
            ("scripts/a.sh", "helper() { :; }\nNAME=one\n"),
            ("scripts/b.sh", "source ./a.sh\nhelper() { :; }\n"),
            (
                "scripts/run.sh",
                "helper() { :; }\nsource ./b.sh\nhelper\ncommand helper\nexec helper\necho helper\n\"$dynamic\"\necho \"$NAME\"\n",
            ),
            ("stray.sh", "helper() { :; }\nNAME=xx"),
        ];
        let (a, n) = bindings(Language::Shell, &files);
        let matched: Vec<_> = n
            .targets
            .iter()
            .filter(|(s, _)| s.file == 2)
            .map(|(s, ts)| (&a[2].calls[s.local], ts[0]))
            .collect();
        assert_eq!(matched.len(), 2);
        for (c, t) in matched {
            assert_eq!(t.file, if c.is_call { 1 } else { 0 })
        }
    }
    #[test]
    fn subshell_function_body_scopes_quotes_and_heredoc() {
        let text = "value=outer\nf() ( local value=inner; echo \"$value\"; )\ncat <<'END'\nghost() { f; }\n$value\nEND\n# f\nf\necho '$value'\necho $value\n";
        let (a, n) = bindings(Language::Shell, &[("run.sh", text)]);
        assert_eq!(
            n.by_name.keys().map(String::as_str).collect::<Vec<_>>(),
            ["f"]
        );
        for s in a[0]
            .calls
            .iter()
            .filter(|s| text[s.span.clone()] == *"value")
        {
            let i = a[0].calls.iter().position(|c| c.span == s.span).unwrap();
            let target = n.targets[&Site { file: 0, local: i }][0];
            assert_eq!(
                a[0].declarations[target.local].span.start,
                if s.owner.is_some() {
                    text.find("value=inner").unwrap()
                } else {
                    0
                }
            );
        }
        assert_eq!(n.targets.len(), 3);
    }
}

impl crate::domain::code::cache::Wire for Evidence{fn save(&self,w:&mut Vec<u8>){match self{Self::Proto(a)=>{ w.push(0); a.save(w);},Self::Shell(a)=>{ w.push(1); a.save(w);}}}fn load(r:&mut &[u8])->std::io::Result<Self>{use crate::domain::code::cache::Wire;match usize::load(r)?{0=>Ok(Self::Proto(Wire::load(r)?)),1=>Ok(Self::Shell(Wire::load(r)?)),_=>Err(crate::domain::code::cache::bad())}}}
