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
function location(node, file, repo) {
  const pos = file.getLineAndCharacterOfPosition(node.getStart(file));
  return path.relative(repo, file.fileName) + ":" + (pos.line + 1) + ":" + (pos.character + 1);
}
function classify(decl) {
  if (ts.isFunctionDeclaration(decl)) return "function";
  if (ts.isMethodDeclaration(decl)) return "method";
  if (ts.isVariableDeclaration(decl) && decl.parent.flags & ts.NodeFlags.Const &&
      decl.initializer && (ts.isArrowFunction(decl.initializer) || ts.isFunctionExpression(decl.initializer))) {
    return "const-function";
  }
  return null;
}
function collect(repo) {
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
  for (const name of names.sort((a, b) => hash(a).localeCompare(hash(b)))) {
    const file = program.getSourceFile(path.join(repo, name));
    if (!file || program.getSyntacticDiagnostics(file).length) continue;
    const candidates = { calls: [], lexical: [] };
    function visit(node) {
      if (ts.isCallExpression(node)) {
        const expr = node.expression;
        const id = ts.isIdentifier(expr) ? expr : ts.isPropertyAccessExpression(expr) ? expr.name : null;
        const symbol = id && checker.getSymbolAtLocation(id);
        const decl = symbol?.declarations?.length === 1 && symbol.declarations[0];
        const kind = decl && classify(decl);
        if (kind && ts.isIdentifier(decl.name) && decl.getSourceFile() === file &&
            ts.isIdentifier(id) && id.text === decl.name.text) {
          // An explicit class receiver or this is readily reviewable for a
          // declared method. Exclude inferred receivers and object-literal
          // methods, which introduce dynamic dispatch ambiguity.
          const classMethod = kind === "method" && ts.isClassDeclaration(decl.parent) &&
              ts.isPropertyAccessExpression(expr) &&
              (expr.expression.kind === ts.SyntaxKind.ThisKeyword ||
               (ts.isIdentifier(expr.expression) && decl.parent.name?.text === expr.expression.text));
          if (classMethod || (kind !== "method" && ts.isIdentifier(expr))) {
            candidates.calls.push({ file: name, kind, name: id.text, use: location(id, file, repo),
              target: location(decl.name, file, repo) });
          }
        }
      }
      // Plain argument, operand and return-value reads. These denote a lexical
      // value even when its runtime value or containing object is unknown.
      if (ts.isIdentifier(node)) {
        const parent = node.parent;
        const read = ts.isReturnStatement(parent) && parent.expression === node ||
          ts.isCallExpression(parent) && parent.arguments.includes(node) ||
          ts.isBinaryExpression(parent) && parent.operatorToken.kind < ts.SyntaxKind.FirstAssignment &&
          (parent.left === node || parent.right === node);
        if (read) {
          const symbol = checker.getSymbolAtLocation(node);
          const decl = symbol?.declarations?.length === 1 && symbol.declarations[0];
          if (decl && (ts.isParameter(decl) || ts.isBindingElement(decl)) &&
              ts.isIdentifier(decl.name) && node.text === decl.name.text && decl.getSourceFile() === file) {
            candidates.lexical.push({ file: name, kind: ts.isParameter(decl) ? "parameter" : "binding-element",
              ownerKind: ts.SyntaxKind[decl.parent.kind],
              anonymousFunction: ts.isFunctionExpression(decl.parent) && !decl.parent.name,
              name: node.text, use: location(node, file, repo), target: location(decl.name, file, repo) });
          }
        }
      }
      ts.forEachChild(node, visit);
    }
    visit(file);
    for (const [family, items] of Object.entries(candidates)) {
      if (!items.length) continue;
      // Select distinct targets, deterministic by source position hash. A single
      // frequently called helper cannot fill an entire file's quota.
      items.sort((a, b) => hash(a.use).localeCompare(hash(b.use)));
      const selected = [];
      const seen = new Set();
      for (const item of items) {
        if (seen.has(item.target)) continue;
        selected.push(item);
        seen.add(item.target);
        if (selected.length === 3) break;
      }
      files[family].push(selected);
    }
  }
  return { scannedFiles: names.length, files };
}
function query(repo, home, op, anchor) {
  const match = /^(.*):(\d+):(\d+)$/.exec(anchor);
  const args = ["--cx", op, match[1], match[2], match[3]];
  const res = command(binary, args, repo, {
    ...process.env, HOME: home, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "./.missing-config",
  });
  if (res.status !== 0) throw new Error(res.stderr);
  const answer = JSON.parse(res.stdout);
  const sites = answer.result.map(r => r.path + ":" + r.line + ":" + r.col);
  return { status: answer.operation_status, sites, truncated: answer.truncated };
}

try {
  const home = path.join(temp, "home");
  fs.mkdirSync(home);
  const inventory = {}, bases = {}, batches = {calls: [], lexical: []};
  for (const [project, fixture] of Object.entries(projects)) {
    const { repo, commit } = checkout(project, fixture);
    bases[project] = repo;
    const { scannedFiles, files } = collect(repo);
    inventory[project] = { commit, scannedFiles, eligibleFiles: Object.fromEntries(Object.entries(files).map(([k, v]) => [k, v.length])) };
    // Select the complete schedule before sending any queries to cx.
    for (const [family, groups] of Object.entries(files)) {
      for (const [i, samples] of groups.entries()) {
        const batch = Math.floor(i / 4);
        (batches[family][batch] ||= []).push(...samples.map(row => ({ ...row, family, project })));
      }
    }
  }
  const waves = [...batches.calls, ...batches.lexical];
  for (const [i, wave] of waves.entries()) {
    let bad = 0, incomplete = 0;
    for (const row of wave) {
      const repo = bases[row.project];
      const forward = query(repo, home, "definition", row.use);
      const reverse = query(repo, home, "references", row.target);
      row.definition = forward;
      row.referencePresent = reverse.sites.includes(row.use);
      row.referenceTruncated = reverse.truncated;
      row.forwardMismatch = forward.status !== "resolved" || forward.sites.length !== 1 || forward.sites[0] !== row.target;
      row.reverseMismatch = !row.referencePresent && !reverse.truncated;
      if (row.forwardMismatch || row.reverseMismatch) bad++;
      if (!row.referencePresent && reverse.truncated) incomplete++;
    }
    console.log(wave[0].family, "wave", i + 1, "samples", wave.length, "mismatched", bad, "uncertain reverse", incomplete);
    fs.mkdirSync(path.dirname(report), { recursive: true });
    fs.writeFileSync(report, JSON.stringify({ typescript: ts.version, inventory, completed: i + 1, waves }, null, 2) + "\n");
  }
  process.exitCode = Number(waves.some(w => w.some(r => r.forwardMismatch || r.reverseMismatch)));
} finally {
  fs.rmSync(temp, { recursive: true, force: true });
}
