const { readFileSync } = require('node:fs');
const { join } = require('node:path');

// Directory boundaries follow the Rust modules. Top-level dispatch and shared
// source stay in cx-core. HTML also consumes the embedded script parser.
const parserConsumers = {
  rust: ['rust'],
  script: ['js', 'python', 'html'],
  direct: ['shell', 'proto']
};

const registryFile = 'src/domain/code/analyzer.rs';

// Read the same registry rows as the Python conformance driver, including
// languages whose fixture directories have yet to be added.
function registryProfiles(root) {
  const source = readFileSync(join(root, registryFile), 'utf8');
  const registry = source.match(/^registry! \{([^]*?)^\}/m);
  if (!registry) throw new Error('missing language registry definition');
  const profiles = Array.from(registry[1].matchAll(/^\s*([A-Za-z][A-Za-z0-9_]*)\s*(?:\[[^\]]*\]\s*)?=>/gm),
    match => match[1].toLowerCase());
  if (!profiles.length) throw new Error('empty language registry');
  if (new Set(profiles).size !== profiles.length) throw new Error('duplicate language registry entries');
  return profiles;
}

exports.createNodesV2 = [registryFile, (files, options, context) => {
  const profiles = registryProfiles(context.workspaceRoot);
  const projects = { src: { root: 'src', name: 'cx-core' } };
  for (const parser of Object.keys(parserConsumers)) {
    const root = 'src/domain/code/' + parser;
    projects[root] = { root, name: 'parser-' + parser };
  }
  for (const language of profiles) {
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
            'Cargo.toml', 'Cargo.lock', 'build.rs', registryFile,
            'tests/language_conformance.py', 'tests/language_response.py',
            'tests/language_fixture_positions.py', 'tools/conformance.cjs', 'nx.json',
            'package.json', 'package-lock.json'
          ].map(path => '{workspaceRoot}/' + path)],
          outputs: ['{workspaceRoot}/' + report]
        }
      }
    };
  }
  return files.map(file => [file, { projects }]);
}];
