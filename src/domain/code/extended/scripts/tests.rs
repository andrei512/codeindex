use super::*;

fn symbols(lang: Language, src: &str) -> Vec<(&str, &'static str)> {
    analyze(lang, src)
        .declarations
        .into_iter()
        .map(|d| (&src[d.span], d.kind))
        .collect()
}
fn edges(lang: Language, src: &str) -> Vec<(&str, Option<&str>, &str)> {
    let a = analyze(lang, src);
    a.direct
        .iter()
        .map(|(i, targets)| {
            let c = &a.calls[*i];
            assert!(c.is_call);
            assert_eq!(targets.len(), 1);
            (
                &src[c.span.clone()],
                c.owner.map(|id| &src[a.declarations[id].span.clone()]),
                &src[a.declarations[targets[0]].span.clone()],
            )
        })
        .collect()
}

#[test]
fn ruby_named_methods_constant_paths_and_opaque_literals() {
    let s = r##"# class Fake
module Outer
  class Outer::Child
    def self.enabled?; true; end
    def value=(v); end
  end
end
s = %q{def nope; end}
re = /class Wrong/
doc = <<~'END'
  def ghost; end
  END
=begin
class AnotherFake
=end
__END__
def trailing; end
"##;
    assert_eq!(
        symbols(Language::Ruby, s),
        [
            ("Outer", "module_declaration"),
            ("Child", "class_definition"),
            ("enabled?", "method_definition"),
            ("value=", "method_definition")
        ]
    );
}

#[test]
fn lua_binds_lexical_locals_but_not_methods_or_shadowed_names() {
    let s = "--[=[ function ghost() end ]=]\nlocal msg=[==[ function decoy() end ]==]\nlocal function real() real() end\nfunction T:run() real() end\nlocal f = function() end\nT:other()";
    assert_eq!(
        symbols(Language::Lua, s),
        [
            ("real", "function_definition"),
            ("run", "function_definition"),
            ("f", "function_definition")
        ]
    );
    assert_eq!(
        edges(Language::Lua, s),
        [
            ("real", Some("real"), "real"),
            ("real", Some("run"), "real")
        ]
    );
    assert!(
        edges(
            Language::Lua,
            "local function x() end\nfunction f(x) x() end"
        )
        .is_empty()
    );
}

#[test]
fn perl_lexical_subs_pod_heredoc_and_quote_operators() {
    let s = "package App::Worker;\nmy sub run { 1 }\nsub m { &run() }\nmy $x=qq{ sub wrong { } };\n$x =~ s{sub old}{sub new};\n=pod\nsub ghost {}\n=cut\nmy $d=<<'HERE';\nsub hidden {}\nHERE\n__DATA__\nsub missing{}";
    assert_eq!(
        symbols(Language::Perl, s),
        [
            ("App::Worker", "package_declaration"),
            ("run", "function_definition"),
            ("m", "function_definition")
        ]
    );
    assert_eq!(edges(Language::Perl, s), [("run", Some("m"), "run")]);
}

#[test]
fn asm_local_labels_direct_branches_and_indirect_exclusions() {
    let s = "entry:\n call helper\n call helper@PLT\n call *%rax\n.asciz \"ghost:; call helper\"\n# fake:\n/* multi:\n   ghost: */\nhelper: ret\n.Ldone: ret";
    assert_eq!(
        symbols(Language::Assembly, s),
        [("entry", "label"), ("helper", "label"), (".Ldone", "label")]
    );
    assert_eq!(
        edges(Language::Assembly, s),
        [("helper", Some("entry"), "helper")]
    );
    assert!(edges(Language::Assembly, "from:\n call dup\ndup: ret\ndup: ret").is_empty());
}

#[test]
fn mako_tags_in_opaque_blocks_python_strings_and_comments() {
    let s = r###"Привет <%def name="render(x)">x</%def>
<%block name='body'/>
<%text><%def name="not_a_def()"></%text>
<%doc><%block name='absent'/></%doc>
## <%def name='ignored()'>
${'<%def name="fake()">' }
<% sample = """%> <%def name="missing()">""" %>
<%namespace file='x.mako' name='extras'/>
"###;
    assert_eq!(
        symbols(Language::Mako, s),
        [
            ("render", "function_definition"),
            ("body", "block_declaration"),
            ("extras", "module_declaration")
        ]
    );
}

#[test]
fn postgres_quoted_and_qualified_routines_dollar_bodies_and_nested_comments() {
    let s = r#"CREATE OR REPLACE FUNCTION app."Λookup"() RETURNS void AS $body$
  CREATE FUNCTION not_real(); $body$ LANGUAGE plpgsql;
-- CREATE FUNCTION fake()
/* outer /*nested*/ CREATE FUNCTION hidden() */
CREATE PROCEDURE sync() LANGUAGE sql AS 'CREATE FUNCTION nope()';
<<retry_block>> BEGIN NULL; END;
"#;
    assert_eq!(
        symbols(Language::PlPgSql, s),
        [
            ("Λookup", "function_definition"),
            ("sync", "procedure_declaration"),
            ("retry_block", "label")
        ]
    );
}

#[test]
fn ruby_singleton_and_perl_package_calls_require_written_receivers_and_unique_targets() {
    let ruby = "class Service\n def self.run(); end\n def self.start(); Service.run(); end\nend\nremote.run()\n";
    assert_eq!(edges(Language::Ruby, ruby), [("run", Some("start"), "run")]);
    assert!(
        edges(
            Language::Ruby,
            &format!("{ruby}\nclass Service; def self.run(); end; end")
        )
        .is_empty()
    );
    let perl = "package One; sub run { 1 } sub a { run(); Two::run(); } package Two; sub run { 2 } sub b { &One::run(); $x->run(); }";
    let a = analyze(Language::Perl, perl);
    let targets = a
        .direct
        .iter()
        .map(|(i, t)| (a.calls[*i].owner.unwrap(), t[0]))
        .collect::<Vec<_>>();
    assert_eq!(targets, [(2, 1), (2, 4), (5, 1)]);
}

#[test]
fn postgres_block_references_stay_in_the_enclosing_routine_and_loop() {
    let sql = "create function f() returns void language plpgsql as $sql$\nbegin <<again>> loop continue AGAIN; exit again; end loop again; exit again; end\n$sql$; create function g() returns int language sql as $$ select '<<fake>> loop' $$;";
    let a = analyze(Language::PlPgSql, sql);
    assert_eq!(
        a.declarations
            .iter()
            .map(|d| &sql[d.span.clone()])
            .collect::<Vec<_>>(),
        ["f", "again", "g"]
    );
    assert_eq!(
        a.calls
            .iter()
            .map(|c| (&sql[c.span.clone()], c.owner, c.is_call))
            .collect::<Vec<_>>(),
        [
            ("AGAIN", Some(0), false),
            ("again", Some(0), false),
            ("again", Some(0), false)
        ]
    );
    assert!(a.direct.iter().all(|(_, ids)| ids == &[1]));
}

#[test]
fn mako_local_namespace_selects_top_level_defs_and_skips_dynamic_inheritance() {
    let s = r#"% if tag == "<%def name='fake()'>":
<%def name="util()">ok</%def>
<%def name="wrapper()"><%def name="hidden()">private</%def>
${local.util()} ${self.util()} ${remote.util()} ${local.hidden()}</%def>"#;
    assert!(!symbols(Language::Mako, s).iter().any(|(s, _)| *s == "fake"));
    assert_eq!(
        edges(Language::Mako, s),
        [("util", Some("wrapper"), "util")]
    );
}

#[test]
fn mixed_sql_preserves_outer_declarations_and_only_opens_explicit_plpgsql() {
    use crate::domain::code::{lexer, markup};
    let s = r#"CREATE TABLE t(id int);
CREATE FUNCTION no_pl(language plpgsql) RETURNS int AS $$ CREATE TABLE ghost(id int); $$ LANGUAGE sql;
CREATE OR REPLACE PROCEDURE app."Refresh"() AS $p$ <<again>> LOOP EXIT again; END LOOP again; $p$ LANGUAGE plpgsql;
CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN END $$;
CREATE VIEW v AS SELECT 'CREATE TABLE invalid(id int)';
"#;
    let mut out = markup::sql(&lexer::tokens(s, Language::Sql));
    sql_routines(s, &mut out);
    let syms = out
        .declarations
        .iter()
        .map(|d| (&s[d.span.clone()], d.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        syms,
        [
            ("t", "object_reference"),
            ("no_pl", "object_reference"),
            ("Refresh", "plpgsql_procedure"),
            ("f", "plpgsql_function"),
            ("v", "object_reference"),
            ("again", "label")
        ]
    );
    assert_eq!(out.calls.len(), 2);
    for (call, targets) in &out.direct {
        assert_eq!(out.calls[*call].owner, Some(2));
        assert_eq!(targets, &[5]);
    }
}
