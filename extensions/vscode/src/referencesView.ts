// One role-labeled reference hit returned by the grouped-references command.
export interface GroupedReferenceItem {
  location: {
    uri: string;
    range: {
      start: { line: number; character: number };
      end: { line: number; character: number };
    };
  };
  role: string;
}

export type GroupedReferencePickRow =
  | {
      kind: 'separator';
      label: string;
    }
  | {
      kind: 'item';
      label: string;
      description: string;
      item: GroupedReferenceItem;
    };

export function groupedReferencePickRows(
  items: readonly GroupedReferenceItem[],
  showRanges: boolean,
  asRelativePath: (uri: string) => string,
): GroupedReferencePickRow[] {
  const rows: GroupedReferencePickRow[] = [];
  let currentRole: string | undefined;
  for (const item of items) {
    if (item.role !== currentRole) {
      currentRole = item.role;
      rows.push({ kind: 'separator', label: item.role });
    }
    const rel = asRelativePath(item.location.uri);
    const line = item.location.range.start.line + 1;
    rows.push({
      kind: 'item',
      label: showRanges ? `${rel}:${line}` : rel,
      description: item.role,
      item,
    });
  }
  return rows;
}
