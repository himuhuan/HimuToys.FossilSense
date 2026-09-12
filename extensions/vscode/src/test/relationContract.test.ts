import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import {
  KnownSiteCount,
  RelationContractValidationError,
  RelationOperation,
  RelationSnapshot,
  mergeKnownSiteCount,
  normalizeRelationRequest,
  normalizeRelationResult,
  sameRelationSnapshot,
} from '../callRelationsModel';
import { RelationDataSource } from '../relationWindow/dataSource';

interface WireFixture {
  name: string;
  producer?: 'rust' | 'typescript';
  kind: 'request' | 'result';
  operation: RelationOperation;
  expectedCode?: string;
  value: unknown;
}

interface GeneratedFixture {
  name: string;
  kind: 'request' | 'result';
  operation: RelationOperation;
  expectedCode: string;
  repeatCodePoint: string;
  repeatCount: number;
}

interface WireSuite {
  contractVersion: number;
  cases: WireFixture[];
  generatedCases?: GeneratedFixture[];
}

interface ScenarioSuite {
  contractVersion: number;
  countMerges: Array<{
    name: string;
    current: KnownSiteCount;
    incoming: KnownSiteCount;
    expected?: KnownSiteCount;
    expectedCode?: string;
  }>;
  snapshotComparisons: Array<{
    name: string;
    left: RelationSnapshot;
    right: RelationSnapshot;
    expected: boolean;
  }>;
  cursorRejections: Array<{ name: string; changedBinding: string; expectedCode: string }>;
  terminalRaces: Array<{
    name: string;
    first: string;
    late: string;
    acceptedTerminalCount: number;
  }>;
}

const fixtureRoot = path.resolve('..', '..', 'tests', 'fixtures', 'relations');
const valid = readJson<WireSuite>('valid.json');
const invalid = readJson<WireSuite>('invalid.json');
const scenarios = readJson<ScenarioSuite>('scenarios.json');

assert.strictEqual(valid.contractVersion, 3);
assert.strictEqual(invalid.contractVersion, 3);
assert.strictEqual(scenarios.contractVersion, 3);

const producers = new Set<string>();
for (const fixture of valid.cases) {
  producers.add(fixture.producer ?? '');
  const normalized = normalizeFixture(fixture);
  assert.deepStrictEqual(
    JSON.parse(JSON.stringify(normalized)),
    fixture.value,
    `${fixture.name} changed on TypeScript round trip`,
  );
}
assert.deepStrictEqual(producers, new Set(['rust', 'typescript']));

const fixtureDataSource: RelationDataSource = {
  prepare: async (_params, _signal) =>
    normalizeRelationResult('prepare', validResultFixture('prepare')),
  expand: async (_params, _signal) =>
    normalizeRelationResult('expand', validResultFixture('expand')),
  evidence: async (_params, _signal) =>
    normalizeRelationResult('evidence', validResultFixture('evidence')),
  locate: async (_params, _signal) =>
    normalizeRelationResult('locate', validResultFixture('locate')),
  onInvalidated: (_listener) => ({ dispose: () => undefined }),
  dispose: () => undefined,
};
assert.strictEqual(typeof fixtureDataSource.prepare, 'function');
assert.strictEqual(typeof fixtureDataSource.onInvalidated, 'function');

for (const fixture of invalid.cases) {
  assertValidationError(fixture.name, fixture.expectedCode!, () => normalizeFixture(fixture));
}

for (const fixture of invalid.generatedCases ?? []) {
  const repeated = fixture.repeatCodePoint.repeat(fixture.repeatCount);
  const value =
    fixture.kind === 'request'
      ? {
          protocolVersion: 3,
          operation: 'prepare',
          params: { root: { kind: 'file', uri: repeated } },
        }
      : {
          protocolVersion: 3,
          outcome: 'error',
          error: {
            code: 'resource_limit',
            message: repeated,
            retryable: false,
            reasonCodes: [],
          },
        };
  assertValidationError(fixture.name, fixture.expectedCode, () =>
    fixture.kind === 'request'
      ? normalizeRelationRequest(value)
      : normalizeRelationResult(fixture.operation, value),
  );
}

for (const fixture of scenarios.countMerges) {
  if (fixture.expected) {
    assert.deepStrictEqual(
      mergeKnownSiteCount(fixture.current, fixture.incoming),
      fixture.expected,
      fixture.name,
    );
  } else {
    assertValidationError(fixture.name, fixture.expectedCode!, () =>
      mergeKnownSiteCount(fixture.current, fixture.incoming),
    );
  }
}

for (const fixture of scenarios.snapshotComparisons) {
  assert.strictEqual(sameRelationSnapshot(fixture.left, fixture.right), fixture.expected, fixture.name);
}

for (const fixture of scenarios.cursorRejections) {
  assert.ok(['nodeRef', 'direction', 'operation', 'pageSize'].includes(fixture.changedBinding));
  assert.strictEqual(fixture.expectedCode, 'invalid_cursor', fixture.name);
}

for (const fixture of scenarios.terminalRaces) {
  assert.notStrictEqual(fixture.first, fixture.late, fixture.name);
  assert.strictEqual(fixture.acceptedTerminalCount, 1, fixture.name);
}

function normalizeFixture(fixture: WireFixture): unknown {
  return fixture.kind === 'request'
    ? normalizeRelationRequest(fixture.value)
    : normalizeRelationResult(fixture.operation, fixture.value);
}

function validResultFixture(operation: RelationOperation): unknown {
  const fixture = valid.cases.find(
    (candidate) => candidate.kind === 'result' && candidate.operation === operation,
  );
  assert.ok(fixture, `missing valid ${operation} result fixture`);
  return fixture.value;
}

function assertValidationError(name: string, expectedCode: string, action: () => unknown): void {
  assert.throws(
    action,
    (error: unknown) =>
      error instanceof RelationContractValidationError && error.code === expectedCode,
    name,
  );
}

function readJson<T>(name: string): T {
  return JSON.parse(fs.readFileSync(path.join(fixtureRoot, name), 'utf8')) as T;
}
