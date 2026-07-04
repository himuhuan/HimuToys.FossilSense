import * as assert from 'assert';
import {
  GroupedReferenceItem,
  GroupedReferencePickRow,
  groupedReferencePickRows,
} from '../referencesView';

const items: GroupedReferenceItem[] = [
  {
    role: 'definition',
    location: {
      uri: 'file:///workspace/src/main.c',
      range: {
        start: { line: 4, character: 2 },
        end: { line: 4, character: 6 },
      },
    },
  },
  {
    role: 'definition',
    location: {
      uri: 'file:///workspace/include/main.h',
      range: {
        start: { line: 0, character: 8 },
        end: { line: 0, character: 12 },
      },
    },
  },
  {
    role: 'read',
    location: {
      uri: 'file:///workspace/src/use.c',
      range: {
        start: { line: 11, character: 14 },
        end: { line: 11, character: 18 },
      },
    },
  },
];

function asRelativePath(uri: string): string {
  return uri.replace('file:///workspace/', '');
}

function visibleRows(rows: GroupedReferencePickRow[]): unknown[] {
  return rows.map((row) => {
    if (row.kind === 'separator') {
      return { kind: row.kind, label: row.label };
    }
    return {
      kind: row.kind,
      label: row.label,
      description: row.description,
      role: row.item.role,
    };
  });
}

function itemRows(
  rows: GroupedReferencePickRow[],
): Extract<GroupedReferencePickRow, { kind: 'item' }>[] {
  return rows.filter((row): row is Extract<GroupedReferencePickRow, { kind: 'item' }> => {
    return row.kind === 'item';
  });
}

assert.deepStrictEqual(visibleRows(groupedReferencePickRows(items, false, asRelativePath)), [
  { kind: 'separator', label: 'definition' },
  { kind: 'item', label: 'src/main.c', description: 'definition', role: 'definition' },
  { kind: 'item', label: 'include/main.h', description: 'definition', role: 'definition' },
  { kind: 'separator', label: 'read' },
  { kind: 'item', label: 'src/use.c', description: 'read', role: 'read' },
]);

assert.deepStrictEqual(visibleRows(groupedReferencePickRows(items, true, asRelativePath)), [
  { kind: 'separator', label: 'definition' },
  { kind: 'item', label: 'src/main.c:5', description: 'definition', role: 'definition' },
  { kind: 'item', label: 'include/main.h:1', description: 'definition', role: 'definition' },
  { kind: 'separator', label: 'read' },
  { kind: 'item', label: 'src/use.c:12', description: 'read', role: 'read' },
]);

const rows = itemRows(groupedReferencePickRows(items, false, asRelativePath));
assert.strictEqual(rows[0].item, items[0]);
assert.deepStrictEqual(rows[0].item.location.range, items[0].location.range);
