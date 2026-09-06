import assert from 'node:assert/strict';

export function fingerprint(bytes) {
  let hash = 0xcbf29ce484222325n;
  for (const byte of bytes) hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
  return 'fnv1a64:' + hash.toString(16).padStart(16, '0');
}

export function stable(value) {
  if (Array.isArray(value)) return '[' + value.map(stable).join(',') + ']';
  if (value && typeof value === 'object') return '{' + Object.keys(value).sort()
    .map(key => JSON.stringify(key) + ':' + stable(value[key])).join(',') + '}';
  return JSON.stringify(value);
}

const tupleKeys = ['name', 'kind', 'role', 'owner', 'scope', 'namespace', 'guard', 'nameRange', 'declarationRange'];
export function tuple(value) {
  return Object.fromEntries(tupleKeys.map(key => [key, value[key] ?? null]));
}
const count = (numerator, denominator) => ({ numerator, denominator,
  value: denominator === 0 ? null : numerator / denominator });

function multiset(items) {
  const result = new Map();
  for (const item of items) {
    const key = stable(tuple(item));
    const entry = result.get(key) ?? { value: tuple(item), count: 0 };
    entry.count++;
    result.set(key, entry);
  }
  return result;
}

// Compare physical source occurrences, preserving duplicate declarations. The
// diagnostic dimension rates use occurrence order; full-tuple multiset equality
// is the pass gate, so a dimension rate cannot conceal a missing/extra entity.
export function compareObservations(wantedObservations, foundObservations, path) {
  assert.ok(Array.isArray(wantedObservations) && Array.isArray(foundObservations));
  const expected = { path, observations: wantedObservations };
  const actual = { observations: foundObservations };
  const wanted = multiset(expected.observations);
  const found = multiset(actual.observations);
  const differences = [];
  let matched = 0;
  for (const [key, entry] of wanted) {
    const actualCount = found.get(key)?.count ?? 0;
    matched += Math.min(entry.count, actualCount);
    if (actualCount < entry.count) differences.push({ path: expected.path, type: 'missing',
      count: entry.count - actualCount, observation: entry.value });
  }
  for (const [key, entry] of found) {
    const expectedCount = wanted.get(key)?.count ?? 0;
    if (entry.count > expectedCount) differences.push({ path: expected.path, type: 'extra',
      count: entry.count - expectedCount, observation: entry.value });
  }
  const byPosition = (left, right) => (left.nameRange?.startByte ?? -1) - (right.nameRange?.startByte ?? -1)
    || stable(tuple(left)).localeCompare(stable(tuple(right)));
  const left = expected.observations.toSorted(byPosition);
  const right = actual.observations.toSorted(byPosition);
  const pairCount = Math.min(left.length, right.length);
  const metrics = { recall: count(matched, left.length), precision: count(matched, right.length) };
  for (const [dimension, keys] of Object.entries({ name: ['name'], kind: ['kind'], role: ['role'],
    scope: ['scope', 'owner', 'namespace'], position: ['nameRange', 'declarationRange'], guard: ['guard'] })) {
    let correct = 0;
    for (let index = 0; index < pairCount; index++) {
      if (keys.every(key => stable(left[index][key] ?? null) === stable(right[index][key] ?? null))) correct++;
    }
    metrics[dimension] = count(correct, pairCount);
  }
  return { pass: differences.length === 0, metrics, differences };
}

export function compareFile(expected, actual) {
  const { metrics, differences } = compareObservations(expected.observations, actual.observations, expected.path);
  const actualCoverage = actual.coverage;
  const expectedCoverage = expected.coverage;
  assert.ok(expectedCoverage && actualCoverage, 'coverage metadata is required');
  for (const key of ['state', 'groups', 'gaps', 'truncated', 'uncoveredRatio']) {
    if (stable(expectedCoverage[key]) !== stable(actualCoverage[key])) differences.push({
      path: expected.path, type: 'coverage', field: key,
      expected: expectedCoverage[key], actual: actualCoverage[key],
    });
  }
  if (expected.languageEvidence && stable(expected.languageEvidence) !== stable(actual.languageEvidence)) {
    differences.push({ path: expected.path, type: 'language_evidence',
      expected: expected.languageEvidence, actual: actual.languageEvidence });
  }
  for (const field of ['sourceHash', 'recoveryRules']) {
    if (stable(expected[field]) !== stable(actual[field])) differences.push({ path: expected.path,
      type: field, expected: expected[field], actual: actual[field] });
  }
  metrics.uncoveredRatio = actualCoverage.truncated ? null : actualCoverage.uncoveredRatio;
  metrics.truncated = actualCoverage.truncated;
  metrics.falseComplete = expectedCoverage.state !== 'complete' && actualCoverage.state === 'complete' ? 1 : 0;
  return { pass: differences.length === 0, metrics, differences };
}

export function validateExport(status, output, runId, manifestHash) {
  assert.equal(status, 0, 'Cargo exporter failed');
  assert.equal(typeof output, 'string', 'export output missing (the named test may not have run)');
  const value = JSON.parse(output);
  assert.equal(value.formatVersion, 1, 'unsupported export format');
  assert.equal(value.runId, runId, 'stale export run id');
  assert.equal(value.manifestHash, manifestHash, 'stale or changed manifest');
  assert.ok(Array.isArray(value.observations), 'export observations must be an array');
  return value;
}

export function validateClangVersion(output, expected) {
  const actual = output.match(/clang version ([0-9.]+)/)?.[1];
  assert.equal(actual, expected, 'Clang version must match the pinned fixture contract');
  return actual;
}
