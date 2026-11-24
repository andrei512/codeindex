const { readFileSync } = require('node:fs');
const { join } = require('node:path');

// Directory boundaries follow the Rust modules. Top-level dispatch and shared
// source stay in cx-core. HTML also consumes the embedded script parser.
const parserConsumers = {
  rust: ['rust'],
  script: ['js', 'python', 'html'],
  direct: ['shell', 'proto']
};

// Use the same registry as the conformance runner, including languages missing fixtures.
exports.createNodesV2 = ['Cargo.toml', (files, options, context) => {
  const registry = readFileSync(join(context.workspaceRoot, 'src/domain/code/mod.rs'), 'utf8');
  const variants = registry.match(/pub enum Language\s*\{([^}]+)\}/)[1];
  const projects = { src: { root: 'src', name: 'cx-core' } };
  for (const parser of Object.keys(parserConsumers)) {
    const root = 'src/domain/code/' + parser;
    projects[root] = { root, name: 'parser-' + parser };
  }
  for (const variant of variants.split(',').map(s => s.trim()).filter(Boolean)) {
    const language = variant.toLowerCase();
    const root = 'tests/fixtures/languages/' + language;
    const report = 'target/conformance/' + language + '.json';
    projects[root] = {
      name: 'lang-' + language,
      implicitDependencies: ['cx-core', ...Object.keys(parserConsumers)
        .filter(parser => parserConsumers[parser].includes(language))
        .map(parser => 'parser-' + parser)],
      root,
      targets: {
        conformance: {
          command: 'python3 tests/language_conformance.py --language ' + language + ' --report ' + report,
          dependsOn: [{ projects: ['codeindex-workspace'], target: 'build-cx' }],
          inputs: ['^default', '{projectRoot}/**/*', ...[
            'Cargo.toml', 'Cargo.lock', 'build.rs',
            'tests/language_conformance.py', 'tools/conformance.cjs', 'nx.json',
            'package.json', 'package-lock.json'
          ].map(path => '{workspaceRoot}/' + path)],
          outputs: ['{workspaceRoot}/' + report]
        }
      }
    };
  }
  return files.map(file => [file, { projects }]);
}];
