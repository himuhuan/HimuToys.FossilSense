import assert from 'node:assert/strict';
import { compareFile, validateExport, validateClangVersion, fingerprint } from './c_frontend_contract.mjs';

const range = (start, end) => ({ startByte: start, endByte: end,
  start: { line: 0, character: start }, end: { line: 0, character: end } });
const observation = { name: 'item', kind: 'object', role: 'definition', owner: null,
  scope: 'persistent', guard: null, nameRange: range(4, 8), declarationRange: range(0, 13) };
const expected = { path: 'fixture.c', sourceHash: 'source-a', recoveryRules: [], observations: [observation], coverage: {
  state: 'complete', groups: { declarations: 'complete' }, gaps: [], truncated: false,
  uncoveredRatio: 0,
} };
const copy = value => structuredClone(value);
assert.equal(compareFile(expected, copy(expected)).pass, true);
for (const mutation of [
  file => { file.observations = []; },
  file => { file.observations.push(copy(observation)); },
  file => { file.observations[0].name = 'wrong'; },
  file => { file.observations[0].kind = 'function'; },
  file => { file.observations[0].role = 'declaration'; },
  file => { file.observations[0].owner = 'Wrong'; },
  file => { file.observations[0].scope = 'request_local'; },
  file => { file.observations[0].namespace = 'tag'; },
  file => { file.observations[0].guard = 'defined(WRONG)'; },
  file => { file.observations[0].nameRange.startByte++; },
  file => { file.observations[0].nameRange.start.character++; },
  file => { file.observations[0].declarationRange.endByte--; },
  file => { file.coverage.state = 'partial'; },
  file => { file.coverage.truncated = true; },
  file => { file.recoveryRules.push('Unexpected'); },
  file => { file.sourceHash = 'stale'; },
]) {
  const actual = copy(expected);
  mutation(actual);
  const result = compareFile(expected, actual);
  assert.equal(result.pass, false, JSON.stringify(actual));
  assert.ok(result.differences.length > 0);
}
const duplicate = copy(expected);
duplicate.observations.push(copy(observation));
assert.equal(compareFile(duplicate, copy(duplicate)).pass, true, 'source redeclarations retain multiplicity');
assert.equal(compareFile(duplicate, expected).pass, false, 'no name-set deduplication');
const empty = copy(expected);
empty.observations = [];
empty.coverage.uncoveredRatio = null;
const metrics = compareFile(empty, copy(empty)).metrics;
for (const key of ['recall', 'precision', 'name', 'kind', 'role', 'scope', 'position', 'guard']) {
  assert.equal(metrics[key].denominator, 0);
  assert.equal(metrics[key].value, null, key + ' has no denominator');
}
const partial = copy(expected);
partial.coverage.state = 'partial';
partial.coverage.groups.declarations = 'partial';
partial.coverage.gaps.push({ reason: 'unknown_macro', evidence: 'macro_invocation', range: range(20, 32) });
partial.coverage.uncoveredRatio = 0.5;
assert.equal(compareFile(partial, expected).metrics.falseComplete, 1);
const truncated = copy(partial);
truncated.coverage.truncated = true;
truncated.coverage.uncoveredRatio = 0.5;
assert.equal(compareFile(partial, truncated).metrics.uncoveredRatio, null, 'truncated denominator is unknown');

const exported = { formatVersion: 1, runId: 'run-a', manifestHash: 'hash-a', observations: [] };
assert.deepEqual(validateExport(0, JSON.stringify(exported), 'run-a', 'hash-a'), exported);
for (const [status, output] of [
  [1, JSON.stringify(exported)], [0, undefined], [0, 'not json'],
  [0, JSON.stringify({ ...exported, runId: 'old' })],
  [0, JSON.stringify({ ...exported, manifestHash: 'old' })],
  [0, JSON.stringify({ ...exported, formatVersion: 99 })],
  [0, JSON.stringify({ ...exported, observations: null })],
]) {
  assert.throws(() => validateExport(status, output, 'run-a', 'hash-a'));
}
assert.equal(fingerprint(Buffer.from('')), 'fnv1a64:cbf29ce484222325');
assert.equal(fingerprint(Buffer.from('hello')), 'fnv1a64:a430d84680aabd0b');
assert.equal(validateClangVersion('clang version 22.1.3\nTarget: x86_64-pc-windows-msvc', '22.1.3'), '22.1.3');
assert.throws(() => validateClangVersion('clang version 22.1.2', '22.1.3'));
assert.throws(() => validateClangVersion('not clang', '22.1.3'));
console.log('C frontend comparator contract tests passed.');
