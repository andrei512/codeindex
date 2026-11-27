//! Project-local evidence for written script source directives and protobuf imports.
use super::cfamily::project::CompileInputs;
use super::{Analysis, Call, Declaration, Kind, Token};
pub(super) mod powershell;
mod proto;
mod shell;
pub(super) mod shell_word;
pub(super) mod starlark;
pub(super) use shell::analyze_with_comments as shell_comments;
pub(super) enum Evidence {
    Translation(Vec<super::cfamily::translation::Partition>),
    Proto(proto::Evidence),
    Shell(shell::Evidence),
    Powershell(powershell::Evidence),
    Csharp(super::cfamily::csharp::Evidence),
    CIncludes(CompileInputs),
}
pub(super) fn analyze_proto(t: &[Token]) -> Analysis {
    proto::analyze(t)
}
pub(super) fn analyze_shell(text: &str) -> Analysis {
    shell::analyze(text)
}
pub(super) use proto::{ProtoGraph, ProtoResolver};
pub(super) use shell::{ShellResolver, SourceEdges};

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
        for (call, t) in matched {
            assert_eq!(t.file, usize::from(call.is_call));
        }
    }
    #[test]
    fn quoted_command_and_handler_names_retain_their_source_span_and_role() {
        let text = "hit() { :; }\n'hit'; \"hit\"; : 'hit'; trap 'hit' EXIT\n";
        let (analyses, names) = bindings(Language::Shell, &[("run.sh", text)]);
        let mut found = Vec::new();
        for (site, targets) in &names.targets {
            let usage = &analyses[0].calls[site.local];
            assert_eq!(&text[usage.span.clone()], "hit");
            assert_eq!(analyses[0].declarations[targets[0].local].span.start, 0);
            found.push((usage.span.start, usage.is_call));
        }
        found.sort_unstable();
        let hits: Vec<_> = text.match_indices("hit").map(|(offset, _)| offset).collect();
        assert_eq!(found, [(hits[1], true), (hits[2], true), (hits[4], false)]);
    }
    #[test]
    fn declaration_inspection_selects_the_requested_namespace_without_calling() {
        let text = "both=old\nboth(){ :; }\ndeclare -xp both\ntypeset -xf both\n";
        let (units, names) = bindings(Language::Shell, &[("run.sh", text)]);
        let unit = &units[0];
        let mut targets = Vec::new();
        for (local, call) in unit.calls.iter().enumerate() {
            assert!(!call.is_call);
            let sites = &names.targets[&Site { file: 0, local }];
            assert_eq!(sites.len(), 1);
            targets.push(unit.declarations[sites[0].local].span.start);
        }
        assert_eq!(targets, [0, text.find("both()").unwrap()]);
    }

    #[test]
    fn shell_nested_assignment_is_recorded_before_its_enclosing_word() {
        let text = "v=${v:=inner}\necho $v\nv=later\necho $v\n";
        let (analysis, names) = bindings(Language::Shell, &[("run.sh", text)]);
        let unit = &analysis[0];
        let mut targets = unit
            .calls
            .iter()
            .enumerate()
            .filter(|(_, call)| &text[call.span.clone()] == "v")
            .map(|(local, _)| {
                let target = names.targets[&Site { file: 0, local }][0];
                unit.declarations[target.local].span.start
            });
        assert_eq!(targets.next(), text.find("v:=inner"));
        assert_eq!(targets.next(), text.find("v=later"));
        assert_eq!(targets.next(), None);
    }
    #[test]
    fn subshell_function_body_scopes_quotes_and_heredoc() {
        let text = "value=outer\nf() ( local value=inner; echo \"$value\"; )\ncat <<'END'\nghost() { f; }\n$value\nEND\n# f\nf\necho '$value'\necho $value\n";
        let (a, n) = bindings(Language::Shell, &[("run.sh", text)]);
        assert_eq!(
            n.by_name.keys().map(String::as_str).collect::<Vec<_>>(),
            ["f"]
        );
        let inner = text.find("value=inner").unwrap();
        for (i, s) in a[0]
            .calls
            .iter()
            .enumerate()
            .filter(|(_, s)| text[s.span.clone()] == *"value")
        {
            let target = n.targets[&Site { file: 0, local: i }][0];
            let expected = s.owner.map_or(0, |_| inner);
            assert_eq!(a[0].declarations[target.local].span.start, expected);
        }
        assert_eq!(n.targets.len(), 3);
    }
}

impl crate::domain::code::cache::Wire for Evidence {
    fn save(&self, w: &mut impl crate::domain::code::cache::Output) {
        match self {
            Self::Translation(partition) => {
                w.push(6);
                partition.save(w);
            }
            Self::Proto(a) => {
                w.push(8);
                a.save(w);
            }
            Self::Shell(a) => {
                w.push(1);
                a.save(w);
            }
            Self::Powershell(a) => {
                w.push(2);
                a.save(w);
            }
            Self::Csharp(a) => {
                w.push(3);
                a.save(w);
            }
            Self::CIncludes(input) => input.save_tagged(w),
        }
    }
    fn load(r: &mut &[u8]) -> std::io::Result<Self> {
        use crate::domain::code::cache::Wire;
        match usize::load(r)? {
            6 => Ok(Self::Translation(Wire::load(r)?)),
            8 => Ok(Self::Proto(Wire::load(r)?)),
            1 => Ok(Self::Shell(Wire::load(r)?)),
            2 => Ok(Self::Powershell(Wire::load(r)?)),
            3 => Ok(Self::Csharp(Wire::load(r)?)),
            tag => Self::load_compatible(tag, r),
        }
    }
}

impl Evidence {
    fn load_compatible(tag: usize, input: &mut &[u8]) -> std::io::Result<Self> {
        match tag {
            0 => proto::Evidence::load_legacy(input).map(Self::Proto),
            4 | 5 | 7 => CompileInputs::load_tagged(tag, input).map(Self::CIncludes),
            _ => Err(crate::domain::code::cache::bad()),
        }
    }
}

impl CompileInputs {
    // Version 4 predates receipts. Version 7 additionally carries the command keys.
    fn save_tagged(&self, out: &mut impl super::cache::Output) {
        use super::cache::Wire;
        out.push(if self.commands.is_some() { 7 } else { 5 });
        self.paths.save(out);
        self.receipts.save(out);
        if let Some(keys) = &self.commands {
            keys.save(out);
        }
    }

    fn load_tagged(tag: usize, input: &mut &[u8]) -> std::io::Result<Self> {
        use super::cache::Wire;
        Ok(Self {
            paths: Wire::load(input)?,
            receipts: if tag >= 5 { Wire::load(input)? } else { Vec::new() },
            commands: (tag == 7).then(|| Wire::load(input)).transpose()?,
        })
    }
}
