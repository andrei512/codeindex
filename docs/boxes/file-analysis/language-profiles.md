---
page-type: reference
---
# Additional language profiles

Use `cx --symbol NAME` to find a searchable declaration. Use `cx --cx definition PATH LINE COL`, `cx --cx references PATH LINE COL`, or `cx --cx callers PATH LINE COL` to follow a relationship. Line and column are one-based; the column counts UTF-16 code units. Use positional navigation for local parameters. These profiles read the captured file directly. Their relationship results have partial coverage. A caller list can omit calls from other files, through an unknown receiver, or through a generated name.

| Language | Selected files | Written names and local navigation |
| --- | --- | --- |
| Dockerfile | Dockerfile, Containerfile, their dotted variants and `.dockerfile` | Named build stages. References include literal stage aliases in FROM, COPY --from and RUN mount sources. |
| Java | `.java` | Named types, constructors and methods. Direct calls can resolve to a unique local declaration. Overload and external receiver lookup remain partial. |
| Makefile | Makefile, makefile, GNUmakefile, dotted variants, `.mk`, `.mak` | Literal rule targets and variables. References include simple prerequisites and variable reads. Direct recursive make recipes can supply callers. |
| Batchfile | `.bat`, `.cmd` | Labels. CALL :label supplies a call and GOTO supplies a reference. Dynamic labels remain unresolved. |
| PHP | `.php`, `.phtml` | Named functions, types and methods in PHP regions. Direct calls are case-insensitive. Dynamic functions and unknown object receivers remain unresolved. |
| CMake | CMakeLists.txt, `.cmake` | Function and macro declarations, parameters and named build targets. Literal commands and supported target arguments can bind locally. |
| Kotlin | `.kt`, `.kts` | Named types, functions and properties. Unique local calls can bind. Imports, overload selection and receiver type inference remain partial. |
| PL/pgSQL | `.plpgsql`, `.pgsql`, and explicit PL/pgSQL routines in `.sql` | Named routines and procedural block labels. Quoted routine names retain their source locations. In `.sql` files, routines with an explicit LANGUAGE plpgsql clause expose procedural symbols alongside ordinary SQL objects. EXIT and CONTINUE can bind to a block label. |
| Ruby | `.rb`, `.rake`, `.gemspec`, Gemfile, Rakefile | Written modules, classes and named method declarations, including singleton method syntax and name suffixes. Literal singleton calls can bind locally. Dynamic method generation and dispatch remain partial. |
| Go | `.go` | Named types, functions and methods, including generic receiver declarations. Direct same-file calls can bind. Imported packages and interface dispatch remain unresolved. |
| Objective-C | `.m` | Written types, methods and functions. A method location points to its first selector component. Self messages can bind to explicitly objc_direct methods by their full selector and method kind. Ordinary dynamic sends remain unresolved. |
| Dart | `.dart` | Named types, methods and functions. Direct same-file calls can bind. Imported and dynamically dispatched calls remain partial. |
| Lua | `.lua` | Named functions, methods and local function assignments. Lexical local functions can bind. A shadowing parameter or dynamic table lookup can prevent binding. |
| Procfile | Procfile and dotted variants | Process type declarations. The command value is an external command line. Callers are unsupported. |
| HCL | `.hcl`, `.tf`, `.tfvars` | Static labeled blocks and attributes. Terraform-style addresses can link to local labels. Generated resources and module loading remain partial. |
| Assembly | `.s`, `.S`, `.asm` | Named labels. Direct call, callq, bl and supported jal instructions can bind. Indirect targets, external relocations and generated labels remain unresolved. |
| Nix | `.nix` | Named bindings and static attributes. Local lexical names and static selections can bind. Imports and dynamic attribute names remain partial. |
| Mako | `.mako`, `.mao` | Named defs, blocks and namespaces. Explicit local template calls can bind. Embedded Python execution and imported template resolution remain partial. |
| Perl | `.pl`, `.pm` | Packages and named subs. Lexical and unique qualified subroutine calls can bind. Dynamic symbol-table lookup and imported subs remain partial. |
| Hack | `.hack`, `.hhi`, and `.php` with a `<?hh` header | Named functions, classes and methods. Literal same-file calls can bind. Runtime dispatch and external dependencies remain partial. |

The list follows reported developer usage in GitHub’s 2026 Q2 language dataset. Sum the `num_pushers` values for each `programming` language across reported economies, excluding the overlapping EU aggregate. Sort descending, then skip languages with existing Codeindex profiles. This selects twenty additional languages and includes build languages classified by GitHub. [Pinned GitHub Innovation Graph data](https://github.com/github/innovationgraph/blob/db6ccdf6a8938e10a4304447e3e302562fda3e64/data/languages.csv)
