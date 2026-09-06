import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { resolve, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash, randomUUID } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { compareFile, compareObservations, fingerprint, validateExport, validateClangVersion } from './c_frontend_contract.mjs';
import { extractClang } from './c_frontend_clang.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const fixtureRoot = join(root, 'crates/fossilsense/tests/fixtures/c_frontend');
const manifestPath = join(fixtureRoot, 'manifest.json');
const runId = 'run-' + randomUUID();
const output = join(root, 'target/c-frontend-conformance', runId);
mkdirSync(output, { recursive: true });
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const report = { formatVersion: 1, runId, startedAt: new Date().toISOString(),
  nodeVersion: process.version, command: process.argv, commands: [], files: [], errors: [], pass: false };

function run(program, args, label, timeout = 120000, env = process.env) {
  const started = Date.now();
  const result = spawnSync(program, args, { cwd: root, env, encoding: 'utf8', windowsHide: true,
    timeout, maxBuffer: 32 * 1024 * 1024 });
  report.commands.push({ command: [program, ...args], label, status: result.status,
    elapsedMs: Date.now() - started, error: result.error?.message ?? null });
  writeFileSync(join(output, label + '.stdout.log'), result.stdout ?? '', 'utf8');
  writeFileSync(join(output, label + '.stderr.log'), result.stderr ?? '', 'utf8');
  return result;
}

function checked(program, args, label, timeout, env) {
  const result = run(program, args, label, timeout, env);
  assert.ok(!result.error && result.status === 0, label + ' failed; see ' + output);
  return result.stdout;
}

try {
  let clang = process.env.FOSSILSENSE_CLANG ?? 'clang';
  for (let index = 2; index < process.argv.length; index++) {
    assert.equal(process.argv[index], '--clang', 'only --clang PATH is supported');
    clang = process.argv[++index];
    assert.ok(clang, '--clang requires a path');
  }
  const manifestBytes = readFileSync(manifestPath);
  const expectedBytes = readFileSync(join(fixtureRoot, 'expected.json'));
  const manifest = JSON.parse(manifestBytes);
  const expected = JSON.parse(expectedBytes);
  assert.equal(manifest.formatVersion, 1);
  assert.ok(manifest.files.length > 0 && manifest.files.length <= 128);
  assert.equal(expected.length, manifest.files.length, 'every fixture needs an explicit expectation');
  report.manifestSha256 = sha256(manifestBytes);
  report.expectedSha256 = sha256(expectedBytes);
  report.manifestHash = fingerprint(manifestBytes);
  report.target = manifest.target;
  report.standard = manifest.standard;
  report.knownContractStubSha256 = sha256(readFileSync(join(fixtureRoot, manifest.stub)));
  report.sourceCommit = checked('git', ['rev-parse', 'HEAD'], 'source-commit', 10000).trim();
  report.sourceStatus = checked('git', ['status', '--porcelain'], 'source-status', 10000).trim();
  report.codeInputs = Object.fromEntries([
    'scripts/c_frontend_contract.mjs', 'scripts/c_frontend_clang.mjs',
    'scripts/verify_c_frontend_conformance.mjs', 'crates/fossilsense/src/parser/tests/conformance.rs',
    'crates/fossilsense/src/parser.rs', 'crates/fossilsense/src/parser/recovery.rs', 'Cargo.lock',
  ].map(path => [path, sha256(readFileSync(join(root, path)))]));
  const version = checked(clang, ['--version'], 'clang-version', 10000);
  validateClangVersion(version, manifest.clangVersion);
  report.clangVersion = version.trim();
  report.clangInstaller = { url: manifest.installerUrl, sha256: manifest.installerSha256 };
  console.log('Exporting Rust observations with Clang ' + manifest.clangVersion + ' pinned for comparison.');
  const exportPath = join(output, 'observations.json');
  const cargo = run('cargo', ['test', '-p', 'fossilsense', '--bin', 'fossilsense',
    'parser::tests::conformance::export_c_frontend_observations', '--', '--exact', '--ignored'],
  'rust-export', 600000, { ...process.env, FOSSILSENSE_CONFORMANCE_MANIFEST: manifestPath,
    FOSSILSENSE_CONFORMANCE_OUTPUT: exportPath, FOSSILSENSE_CONFORMANCE_RUN_ID: runId });
  const exported = validateExport(cargo.error ? null : cargo.status,
    existsSync(exportPath) ? readFileSync(exportPath, 'utf8') : undefined, runId, report.manifestHash);
  assert.equal(exported.observations.length, expected.length, 'export fixture count mismatch');
  for (let index = 0; index < manifest.files.length; index++) {
    const fixture = manifest.files[index];
    const wanted = expected[index];
    const actual = exported.observations[index];
    assert.equal(wanted.path, fixture.path);
    assert.equal(actual.path, fixture.path);
    const path = resolve(fixtureRoot, fixture.path);
    assert.equal(dirname(path), fixtureRoot, 'fixtures must be immediate source files');
    const source = readFileSync(path);
    assert.ok(source.length <= 1024 * 1024);
    assert.equal(actual.sourceHash, fingerprint(source), 'source changed after Rust export');
    const contract = compareFile(wanted, actual);
    const file = { path: fixture.path, sourceSha256: sha256(source), oracle: fixture.oracle,
      observationScopes: fixture.observationScopes, contract, clang: [] };
    report.files.push(file);
    assert.ok(['clang', 'manual'].includes(fixture.oracle), 'fixture oracle must be explicit');
    if (fixture.oracle === 'clang') {
      for (const configuration of fixture.clangConfigurations) {
        const args = ['-x', 'c', '-std=' + manifest.standard, '--target=' + manifest.target,
          '-nostdinc', '-include', join(fixtureRoot, manifest.stub), ...configuration.args,
          '-Xclang', '-ast-dump=json', '-fsyntax-only', path];
        const label = 'clang-' + fixture.path.replaceAll('.', '-') + '-' + configuration.id;
        const ast = JSON.parse(checked(clang, args, label));
        const unsupported = wanted.coverage.gaps.filter(gap => gap.evidence === 'scope_not_supported').map(gap => gap.range);
        const projected = extractClang(ast, source, path, unsupported);
        // Clang removes preprocessor directives. The manual/Rust comparison
        // verifies raw guard text; the oracle independently checks each active
        // branch's complete source declaration set under explicit -D/-U flags.
        const active = wanted.observations.filter(item => configuration.activeGuards.includes(item.guard))
          .map(item => ({ ...item, guard: null }));
        const result = compareObservations(active, projected.observations, fixture.path);
        result.metrics.guard = { numerator: 0, denominator: 0, value: null };
        file.clang.push({ configuration: configuration.id, args, result, excluded: projected.excluded });
      }
    }
    console.log(fixture.path + ': ' + (contract.pass && file.clang.every(run => run.result.pass) ? 'PASS' : 'FAIL'));
  }
  const supported = report.files.filter((_, index) => expected[index].coverage.state === 'complete');
  report.quality = { supportedFiles: supported.length, measuredFiles: report.files.length,
    falseComplete: report.files.reduce((sum, file) => sum + file.contract.metrics.falseComplete, 0),
    unsupportedRegionsExplicit: report.files.filter(file => file.observationScopes.includes('unsupported_region')).length,
    supportedUncoveredZero: supported.every(file => !file.contract.metrics.truncated &&
      (file.contract.metrics.uncoveredRatio === 0 || (file.contract.metrics.recall.denominator === 0 && file.contract.metrics.uncoveredRatio === null))),
  };
  for (const dimension of ['recall', 'precision', 'name', 'kind', 'role', 'scope', 'position', 'guard']) {
    const numerator = supported.reduce((sum, file) => sum + file.contract.metrics[dimension].numerator, 0);
    const denominator = supported.reduce((sum, file) => sum + file.contract.metrics[dimension].denominator, 0);
    report.quality[dimension] = { numerator, denominator, value: denominator ? numerator / denominator : null };
  }
  report.pass = report.files.every(file => file.contract.pass && file.clang.every(run => run.result.pass))
    && report.quality.falseComplete === 0 && report.quality.supportedUncoveredZero;
} catch (error) {
  report.errors.push({ message: error.message, stack: error.stack });
  console.error(error.message);
} finally {
  report.completedAt = new Date().toISOString();
  writeFileSync(join(output, 'report.json'), JSON.stringify(report, null, 2) + '\n', 'utf8');
  console.log('conformance_report: ' + join(output, 'report.json'));
  process.exitCode = report.pass ? 0 : 1;
}
