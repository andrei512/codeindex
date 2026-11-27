// Differential sample of intra-file call and lexical navigation in pinned public code.
// Usage: node tests/language_script_oracle.cjs [path/to/typescript] [path/to/cx]
// From a clean project checkout:
//   cargo build --release --locked
//   npm install --prefix target/oracle-tools --no-save --ignore-scripts typescript@5.9.3
//   node tests/language_script_oracle.cjs ./target/oracle-tools/node_modules/typescript
// A missing mirror triggers a fetch of the exact public commit. No upstream
// project install, build, config script, or compiler plugin is executed.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");

const ts = require(process.argv[2] ? path.resolve(process.argv[2]) : "typescript");
if (ts.version !== "5.9.3") throw new Error("This answer key requires TypeScript 5.9.3");
const root = path.resolve(__dirname, "..");
const binary = path.resolve(process.argv[3] || path.join(root, "target/release/cx"));
const temp = fs.mkdtempSync(path.join(os.tmpdir(), "cx-typescript-oracle-"));
const report = path.join(root, "target/language-script-oracle.json");
const projects = {
  humanizer: "csharp/public-humanizer",
  benchmarks: "go/public-benchmarks",
  django: "python/public-django",
};

function command(exe, args, cwd, env = process.env) {
  const result = spawnSync(exe, args, { cwd, env, encoding: "utf8", timeout: 120000, maxBuffer: 8 << 20 });
  if (result.error) throw result.error;
  return result;
}
function git(args, cwd) {
  const res = command("git", args, cwd);
  if (res.status !== 0) throw new Error(res.stderr);
  return res.stdout;
}
function hash(s) {
  return crypto.createHash("sha256").update(s).digest("hex");
}
function checkout(project, fixture) {
  const spec = JSON.parse(fs.readFileSync(path.join(root, "tests/fixtures/languages", fixture, "public.json")));
  const mirror = path.join(root, "target/conformance-cache", hash(spec.url));
  const repo = path.join(temp, project);
  fs.mkdirSync(repo);
  git(["init", "-q"], repo);
  const cached = fs.existsSync(mirror) && command("git", ["cat-file", "-e", spec.commit + "^{commit}"], mirror).status === 0;
  git(["fetch", "-q", "--depth=1", cached ? mirror : spec.url, spec.commit], repo);
  git(["checkout", "-q", "--detach", "FETCH_HEAD"], repo);
  return { repo, commit: spec.commit };
}
function location(node, file, name) {
  const pos = file.getLineAndCharacterOfPosition(node.getStart(file));
  return name + ":" + (pos.line + 1) + ":" + (pos.character + 1);
}
function classify(decl) {
  if (ts.isFunctionDeclaration(decl)) return "function";
  if (ts.isMethodDeclaration(decl)) return "method";
  if (!ts.isVariableDeclaration(decl) || !(decl.parent.flags & ts.NodeFlags.Const)) return null;
  const initializer = decl.initializer;
  if (!initializer) return null;
  return ts.isArrowFunction(initializer) || ts.isFunctionExpression(initializer) ? "const-function" : null;
}
function localDeclaration(id, checker, file) {
  if (!id || !ts.isIdentifier(id)) return null;
  const declarations = checker.getSymbolAtLocation(id)?.declarations;
  if (declarations?.length !== 1) return null;
  const decl = declarations[0];
  return decl.name && ts.isIdentifier(decl.name) && decl.name.text === id.text &&
    decl.getSourceFile() === file ? decl : null;
}
function lexicalRead(node) {
  const parent = node.parent;
  return ts.isReturnStatement(parent) && parent.expression === node ||
    ts.isCallExpression(parent) && parent.arguments.includes(node) ||
    ts.isBinaryExpression(parent) && parent.operatorToken.kind < ts.SyntaxKind.FirstAssignment &&
    (parent.left === node || parent.right === node);
}
function methodReceiver(expr, decl) {
  if (!ts.isPropertyAccessExpression(expr) || !ts.isClassDeclaration(decl.parent)) return false;
  const receiver = expr.expression;
  return receiver.kind === ts.SyntaxKind.ThisKeyword ||
    ts.isIdentifier(receiver) && decl.parent.name?.text === receiver.text;
}
function callSite(expr, checker, file, site) {
  const member = ts.isPropertyAccessExpression(expr);
  const id = member ? expr.name : expr;
  const decl = localDeclaration(id, checker, file);
  const kind = decl && classify(decl);
  if (!kind) return null;
  if (kind === "method" ? !methodReceiver(expr, decl) : member) return null;
  return site(id, decl, kind);
}
function lexicalKind(decl) {
  if (!decl) return null;
  if (ts.isParameter(decl)) return "parameter";
  if (ts.isBindingElement(decl)) return "binding-element";
  return null;
}
function lexicalSite(node, checker, file, site) {
  const decl = localDeclaration(node, checker, file);
  const kind = lexicalKind(decl);
  if (!kind) return null;
  return site(node, decl, kind, {
    ownerKind: ts.SyntaxKind[decl.parent.kind],
    anonymousFunction: ts.isFunctionExpression(decl.parent) && !decl.parent.name,
  });
}

// Compute each stable sample rank once before sorting.
function rankByHash(items, key) {
  return items.map(item => [hash(key(item)), item])
    .sort(([a], [b]) => a.localeCompare(b)).map(([, item]) => item);
}

function* firstUsePerTarget(items) {
  const seen = new Set();
  for (const item of items) {
    if (seen.has(item.target)) continue;
    seen.add(item.target);
    yield item;
  }
}

function distinctTargets(items) {
  const selected = [];
  for (const item of firstUsePerTarget(rankByHash(items, item => item.use))) {
    selected.push(item);
    if (selected.length === 3) break;
  }
  return selected;
}

function sourceCandidates(checker, file, name) {
  const candidates = { calls: [], lexical: [] };
  const targets = new Map();
  function bindingSite(id, decl, kind, owner) {
    if (!targets.has(decl)) targets.set(decl, location(decl.name, file, name));
    return { file: name, kind, ...owner, name: id.text, use: location(id, file, name), target: targets.get(decl) };
  }
  function admit(node) {
    if (ts.isCallExpression(node)) {
      return ["calls", callSite(node.expression, checker, file, bindingSite)];
    }
    if (ts.isIdentifier(node) && lexicalRead(node)) {
      return ["lexical", lexicalSite(node, checker, file, bindingSite)];
    }
    return [null, null];
  }
  function visit(node) {
    const [family, site] = admit(node);
    if (site) candidates[family].push(site);
    ts.forEachChild(node, visit);
  }
  visit(file);
  return candidates;
}

function collect(repo, witnesses) {
  const names = git(["ls-files", "-z"], repo).split("\0").filter(name =>
    /\.[cm]?[jt]sx?$/.test(name) && !/\.d\.[cm]?ts$|\.min\.|(?:^|\/)(?:vendor|node_modules|dist|build|generated)\//.test(name) &&
    fs.statSync(path.join(repo, name)).size <= 150000);
  // Force file-local scopes. Only use targets whose declaration shares the use's
  // source file; this witness does not require loading external project packages.
  const program = ts.createProgram(names.map(n => path.join(repo, n)), {
    allowJs: true, noLib: true, noResolve: true, target: ts.ScriptTarget.ESNext,
    jsx: ts.JsxEmit.Preserve, moduleDetection: ts.ModuleDetectionKind.Force,
  });
  const checker = program.getTypeChecker();
  const files = { calls: [], lexical: [] };
  for (const name of rankByHash(names, name => name)) {
    const file = program.getSourceFile(path.join(repo, name));
    if (!file || program.getSyntacticDiagnostics(file).length) continue;
    recordCandidates(sourceCandidates(checker, file, name), witnesses, files);
  }
  return { scannedFiles: names.length, files };
}
function recordCandidates(candidates, witnesses, files) {
  for (const [family, items] of Object.entries(candidates)) {
    if (!items.length) continue;
    recordWitnesses(items, witnesses);
    files[family].push(distinctTargets(items));
  }
}

function recordWitnesses(items, witnesses) {
  // Preserve every independently known site before applying each file's quota.
  for (const row of items) {
    const prior = witnesses.get(row.use);
    witnesses.set(row.use, prior === undefined || prior === row.target ? row.target : null);
  }
}

function schedule(batches, files, project) {
  for (const [family, groups] of Object.entries(files)) {
    for (const [i, samples] of groups.entries()) {
      const batch = Math.floor(i / 4);
      (batches[family][batch] ||= []).push(...samples.map(row => ({ ...row, family, project })));
    }
  }
}

function makePlan() {
  const home = path.join(temp, "home");
  fs.mkdirSync(home);
  const inventory = {}, bases = {}, witnesses = {}, batches = {calls: [], lexical: []};
  for (const [project, fixture] of Object.entries(projects)) {
    const { repo, commit } = checkout(project, fixture);
    bases[project] = repo;
    const known = new Map();
    const { scannedFiles, files } = collect(repo, known);
    witnesses[project] = Object.fromEntries(known);
    inventory[project] = { commit, scannedFiles, eligibleFiles: Object.fromEntries(Object.entries(files).map(([k, v]) => [k, v.length])) };
    // Select the complete schedule before sending any queries to cx.
    schedule(batches, files, project);
  }
  const waves = [...batches.calls, ...batches.lexical];
  // The pinned compiler, projects and sampling rule define these exact nine waves.
  const sites = waves.map(wave => wave.map(row => [row.project, row.family, row.use, row.target]));
  if (hash(JSON.stringify(sites)) !== "13469080af2664430514264231d4b9da1bf0d1daca0906cfd1374a9c5b4b2e2c") {
    throw new Error("TypeScript oracle sample differs from the pinned schedule");
  }
  return {typescript: ts.version, inventory, bases, home, waves, witnesses};
}

try {
  const plan = makePlan();
  const runner = spawnSync("python3", [path.join(__dirname, "language_script_oracle.py"), binary, report], {
    input: JSON.stringify(plan), stdio: ["pipe", "inherit", "inherit"],
  });
  if (runner.error) throw runner.error;
  process.exitCode = runner.status ?? 1;
} finally {
  fs.rmSync(temp, { recursive: true, force: true });
}
