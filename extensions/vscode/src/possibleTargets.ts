export interface PossibleTargetLocation {
  uri: string;
  range: {
    start: { line: number; character: number };
    end: { line: number; character: number };
  };
}

export interface PossibleTargetItem {
  location: PossibleTargetLocation;
  name: string;
  kind: string;
  role: string;
  scopeTier: string;
  linkage: string;
  guard?: string | null;
  signature: string;
  reason: string;
  visibility: string;
  source: string;
  confidence: string;
  arityCompatibility?: string | null;
  pairingEvidence?: string | null;
  origin: string;
}

export interface PossibleTargetsCoverage {
  bounded: boolean;
  limit: number;
  scanned: number;
  truncated: boolean;
  open: boolean;
  openReason?: string | null;
  incompleteReason?: string | null;
  semanticGeneration: number;
  overlayEpoch: number;
  resolverVersion: number;
}

export interface PossibleTargetsResponse {
  protocolVersion: number;
  name: string;
  items: PossibleTargetItem[];
  coverage: PossibleTargetsCoverage;
}

export type PossibleTargetPickRow =
  | { kind: 'separator'; label: string }
  | {
      kind: 'item';
      label: string;
      description: string;
      detail: string;
      item: PossibleTargetItem;
    };

const ROLE_ORDER = new Map<string, number>([
  ['definition', 0],
  ['tentative_definition', 1],
  ['declaration', 2],
  ['unknown_declaration_or_definition', 3],
]);

const VISIBILITY_ORDER = new Map<string, number>([
  ['current_visible', 0],
  ['reachable', 1],
  ['external_first_layer', 2],
  ['uncertain', 3],
  ['not_currently_visible', 4],
  ['workspace_fallback', 5],
]);

export function possibleTargetPickRows(
  items: readonly PossibleTargetItem[],
  asRelativePath: (uri: string) => string,
): PossibleTargetPickRow[] {
  const sorted = [...items].sort((left, right) => {
    const role = orderOf(ROLE_ORDER, left.role) - orderOf(ROLE_ORDER, right.role);
    if (role !== 0) {
      return role;
    }
    const visibility =
      orderOf(VISIBILITY_ORDER, left.visibility) -
      orderOf(VISIBILITY_ORDER, right.visibility);
    if (visibility !== 0) {
      return visibility;
    }
    const path = left.location.uri.localeCompare(right.location.uri);
    if (path !== 0) {
      return path;
    }
    return left.location.range.start.line - right.location.range.start.line;
  });

  const rows: PossibleTargetPickRow[] = [];
  let section: string | undefined;
  for (const item of sorted) {
    const nextSection = `${humanize(item.role)} · ${humanize(item.visibility)}`;
    if (nextSection !== section) {
      section = nextSection;
      rows.push({ kind: 'separator', label: nextSection });
    }
    const path = asRelativePath(item.location.uri);
    const line = item.location.range.start.line + 1;
    const evidence = [item.reason, item.pairingEvidence, item.arityCompatibility]
      .filter((part): part is string => Boolean(part))
      .map(humanize)
      .join(' · ');
    const qualifiers = [
      `${humanize(item.scopeTier)} scope`,
      `${humanize(item.linkage)} linkage`,
      item.guard ? `guard: ${item.guard}` : undefined,
      evidence || undefined,
    ].filter((part): part is string => Boolean(part));
    rows.push({
      kind: 'item',
      label: `${path}:${line}`,
      description: item.signature || humanize(item.kind),
      detail: qualifiers.join(' · '),
      item,
    });
  }
  return rows;
}

export function possibleTargetsCoverageSummary(
  coverage: PossibleTargetsCoverage,
): string {
  const notes: string[] = [];
  if (coverage.bounded) {
    notes.push(`bounded recall (limit ${coverage.limit})`);
  }
  if (coverage.truncated) {
    notes.push('results truncated');
  }
  if (coverage.open) {
    notes.push(
      coverage.openReason
        ? `coverage open: ${humanize(coverage.openReason)}`
        : 'coverage open',
    );
  }
  if (coverage.incompleteReason) {
    notes.push(`incomplete: ${humanize(coverage.incompleteReason)}`);
  }
  return notes.join(' · ');
}

function orderOf(order: ReadonlyMap<string, number>, value: string): number {
  return order.get(value) ?? Number.MAX_SAFE_INTEGER;
}

function humanize(value: string): string {
  return value.replace(/_/g, ' ');
}
