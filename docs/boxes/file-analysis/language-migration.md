---
page-type: task
---
# Migrate language ownership

Use this procedure to move the current language implementations behind the [ownership contracts](language-ownership.md). The migration unit is a working query path for one family. Keep its externally observable behavior measurable while changing who owns each decision.

## Establish the comparison

Freeze a release binary, registry inventory and representative fixture for each profile and dialect. Record path selection, declaration and use byte spans, symbol visibility, forward and reverse results, unresolved reasons, role classification, score components and public CLI output wherever supported. Record the generation compatibility version. Compare a fresh build, reopened index and edit against a fresh rebuild of the same final checkout. Use independent expected answers for intended bug fixes. Store those exceptions with the fixture.

## Move the contracts in dependency order

1. **Registry and request.** Route existing entry points through a descriptor per profile. Start from the file routing in `domain/code/mod.rs`, navigation capabilities in `navigation.rs`, and product selection in `application/index/build/local.rs`. Carry written and effective modes, context and requested products. Initially delegate to the old bodies. Test exact filenames, extensions, content overrides and unknown input.
2. **Project inputs and reuse.** Move C/C++ configuration and edit classification out of `application/index/build/{c_project,script_edit}.rs` behind the project and proof operations. Retain the existing storage writer, memory budget and scheduler. Exercise a configuration-only switch, creation of a previously missing higher-priority header, and an edit that shifts spans. Check unchanged callers and stale reverse edges. Replace concrete scheduler branches as their real callers switch.
3. **File analysis and scoring.** Move one family at a time from `domain/code/{lexer,scoring,roles}.rs` and the family parsers. Bring the scoring and code-only entry points through the same concrete analyzer. Preserve input sharing, path-sensitive roles and the hidden score comparison mode. For shared walks, move the grammatical decision into the concrete dialect. Use direct implementations for formats whose syntax needs a different walk.
4. **Binding and queries.** Replace the resolver calls wired into `ForwardNames::resolve` with registered providers and selected work. Keep local ordinals, global site identity, deterministic ordering and coverage. Compare duplicate names, cyclic imports, missing providers, shadowing, and the exact forward and reverse edges after deletion or rename. Move navigation metadata to the descriptor and combine it with per-document observations.
5. **Retire old routes.** Remove a legacy branch when every caller for that family uses the new owner and passes the comparison. Check the published baseline index opens correctly under the candidate. If evidence layout or semantics change, exercise migration or targeted invalidation, reopen, and compare against a clean candidate build.

## Family acceptance cases

Together these groups cover the 34 language variants currently selected by the code registry. Dialects and non-code inputs add cases beyond that count. The 17 archived modes in the [binding profiles](../binding/profiles.md) describe a separate conformance inventory.

- **C, C++, C#, Swift.** Reuse the existing declaration dialect pattern for functions, parameters and overload decisions. Include an ambiguous overload, C++ configuration for a C file, and C# project membership. Run the same header in two C++ translation contexts with different macro definitions and forced includes. Verify each context sees only its active declarations. Share captured bytes, share syntax only when its inputs agree, and preserve distinct contextual bindings. Test score results for the written language separately.
- **JavaScript and Python.** Cover JS, JSX, TS, TSX and Python stubs. Check aliased imports, shadowing and cross-file edits. A callback use may resolve to the parameter while its dynamic implementation remains unknown.
- **Rust.** Preserve delimiter recovery, Cargo context and the end-of-file span patch. Exercise a manifest-only edit and a change that preserves the graph while changing source positions.
- **Go, Java, Kotlin, Objective-C, Dart, PHP, Hack.** Preserve the existing declaration, shadow and reference grammar specializations. Include qualified receivers, Objective-C selectors, Go imports and a Hack header in PHP-routed source.
- **Ruby, Lua, Perl, Assembly, Mako, PL/pgSQL.** Check scope and shadowing, case and label syntax, and Mako host versus embedded Python visibility. Exercise positive and negative role (`cx --score --roles FILE --json`), score and symbol requests for the same path. A Perl test identified by content in a `.t` file can contribute a score without acquiring a new symbol-search capability.
- **Dockerfile, Makefile, Batchfile, CMake, Procfile, HCL, Nix.** Include extensionless inputs, stage references, Make expansion, literal labels, heredocs and guest test evidence. A Procfile process is searchable and its external command does not establish a caller edge.
- **HTML, CSS, SQL.** Check embedded HTML source offsets, declaration visibility, external scripts and CSS scope. In SQL distinguish ordinary objects from explicitly procedural routines. Navigation capability can depend on extracted declarations.
- **Shell, PowerShell, Proto, Starlark.** Include Bash path routing, role-only Bats recognition, source order, case-insensitive module commands, public imports and BUILD filenames. Exercise unavailable operations as well as successful bindings.
- **Markdown and ordinary text.** Preserve heading sections, backlinks, ranking and source search. A source with no recognized language must still answer supported text queries.

## Decide whether to switch

Run the affected fixtures through the real CLI and compare saved evidence where the renderer can hide a difference. Use the [conformance procedure](../../conformance.md) for the relevant public oracle and repository transitions. Run affected score and role dev evaluations when changing their producers. Keep blind scoring data sealed. At integration, run the full registry and mixed-repository suite.

Measure fresh build through durable reopen, a one-file update, a dependency-only update and warm navigation on identical fixed inputs. For scoring or role changes also measure warm score, role lookup, re-lexes and project-input reads. Alternate baseline and candidate runs. Record wall time, peak resident memory, source captures, syntax extractions and binding work by product key. Require equivalent answers and investigate regressions outside baseline variation. A higher maintainability score alone does not justify a slower or less accurate query.
