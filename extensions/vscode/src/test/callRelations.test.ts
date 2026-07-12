import * as assert from 'assert';
import {
  CallRelation,
  coverageSummary,
  evidenceSummary,
  normalizeRichRelationResponse,
  relationEntity,
} from '../callRelationsModel';

const anchor = {
  path: 'src/main.c',
  name: 'caller',
  qualifiedName: 'caller',
  signature: { normalized: '(void)' },
  nameRange: {
    start: { line: 0, character: 4 },
    end: { line: 0, character: 10 },
    startByte: 4,
    endByte: 10,
  },
  declarationRange: {
    start: { line: 0, character: 0 },
    end: { line: 0, character: 20 },
    startByte: 0,
    endByte: 20,
  },
  entityKey: 'caller-key',
};

const caller = {
  entityKey: 'caller-key',
  name: 'caller',
  qualifiedName: 'caller',
  signature: { normalized: '(void)' },
  primaryAnchor: anchor,
};

const callee = {
  ...caller,
  entityKey: 'callee-key',
  name: 'callee',
  qualifiedName: 'ns::callee',
  primaryAnchor: { ...anchor, name: 'callee', qualifiedName: 'ns::callee' },
};

const relation: CallRelation = {
  caller,
  callee,
  direction: 'outgoing',
  callSites: [],
  confidence: 'medium',
  evidence: {
    supports: ['same_file', 'compatible_arity'],
    contradictions: [],
    unknowns: ['open_include_scope'],
  },
};

assert.strictEqual(relationEntity(relation, 'incoming'), caller);
assert.strictEqual(relationEntity(relation, 'outgoing'), callee);
assert.strictEqual(
  evidenceSummary(relation),
  'supports: same_file, compatible_arity · unknowns: open_include_scope',
);
assert.strictEqual(
  coverageSummary({
    eligibleFiles: 12,
    analyzedFiles: 10,
    fallbackFiles: 2,
    externalBodiesLimited: true,
    semanticGeneration: 7,
  }),
  '10/12 files analyzed, 2 fallback; external bodies are declaration-only',
);

const normalized = normalizeRichRelationResponse({
  protocolVersion: 2,
  revision: {
    engineEpoch: 1,
    semanticGeneration: 7,
    overlayEpoch: 0,
    resolverVersion: 2,
  },
  entities: { '1': caller, '2': callee },
  relations: [
    {
      callerId: 1,
      calleeId: 2,
      direction: 'outgoing',
      callSites: [],
      confidence: 'medium',
      evidence: relation.evidence,
    },
  ],
  complete: false,
  budgetState: 'page_limited',
  coverage: {
    eligibleFiles: 12,
    analyzedFiles: 10,
    fallbackFiles: 2,
    externalBodiesLimited: true,
    semanticGeneration: 7,
    incompleteReason: 'page_limit',
  },
  nextCursor: '7.0.2.o.c8',
});
assert.strictEqual(normalized.relations[0].caller, caller);
assert.strictEqual(normalized.relations[0].callee, callee);
