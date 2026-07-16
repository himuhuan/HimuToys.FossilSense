import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import {
  PossibleTargetItem,
  PossibleTargetPickRow,
  possibleTargetPickRows,
  possibleTargetsCoverageSummary,
} from '../possibleTargets';

function item(
  role: string,
  visibility: string,
  uri: string,
  line: number,
): PossibleTargetItem {
  return {
    location: {
      uri,
      range: {
        start: { line, character: 4 },
        end: { line, character: 7 },
      },
    },
    name: 'foo',
    kind: 'function',
    role,
    scopeTier: visibility === 'reachable' ? 'reachable' : 'current',
    linkage: 'external',
    guard: null,
    signature: 'int foo(void)',
    reason: visibility === 'reachable' ? 'reachable_include' : 'current_file',
    visibility,
    source: 'workspace',
    confidence: 'reachable',
    arityCompatibility: 'compatible',
    pairingEvidence: 'strict_one_to_one',
    origin: 'indexed',
  };
}

function visibleRows(rows: PossibleTargetPickRow[]): unknown[] {
  return rows.map((row) =>
    row.kind === 'separator'
      ? { kind: row.kind, label: row.label }
      : {
          kind: row.kind,
          label: row.label,
          description: row.description,
          role: row.item.role,
          visibility: row.item.visibility,
        },
  );
}

const rows = possibleTargetPickRows(
  [
    item('declaration', 'current_visible', 'file:///workspace/include/foo.h', 2),
    item('definition', 'reachable', 'file:///workspace/src/foo.c', 8),
    item('definition', 'not_currently_visible', 'file:///workspace/src/late.c', 20),
  ],
  (uri) => uri.replace('file:///workspace/', ''),
);

assert.deepStrictEqual(visibleRows(rows), [
  { kind: 'separator', label: 'definition · reachable' },
  {
    kind: 'item',
    label: 'src/foo.c:9',
    description: 'int foo(void)',
    role: 'definition',
    visibility: 'reachable',
  },
  { kind: 'separator', label: 'definition · not currently visible' },
  {
    kind: 'item',
    label: 'src/late.c:21',
    description: 'int foo(void)',
    role: 'definition',
    visibility: 'not_currently_visible',
  },
  { kind: 'separator', label: 'declaration · current visible' },
  {
    kind: 'item',
    label: 'include/foo.h:3',
    description: 'int foo(void)',
    role: 'declaration',
    visibility: 'current_visible',
  },
]);

assert.strictEqual(
  possibleTargetsCoverageSummary({
    bounded: true,
    limit: 256,
    scanned: 256,
    truncated: true,
    open: true,
    openReason: 'ambiguous_include',
    incompleteReason: 'facts_unavailable',
    semanticGeneration: 7,
    overlayEpoch: 3,
    resolverVersion: 5,
  }),
  'bounded recall (limit 256) · results truncated · coverage open: ambiguous include · incomplete: facts unavailable',
);

const packageJson = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'package.json'), 'utf8'),
);
assert.ok(
  packageJson.contributes.commands.some(
    (entry: { command?: string }) => entry.command === 'fossilsense.findAllPossibleTargets',
  ),
);
assert.ok(
  packageJson.contributes.menus['editor/context'].some(
    (entry: { command?: string }) => entry.command === 'fossilsense.findAllPossibleTargets',
  ),
);
