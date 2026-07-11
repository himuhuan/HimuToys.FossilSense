export type RelationDirection = 'incoming' | 'outgoing';

export interface SourcePosition {
  line: number;
  character: number;
}

export interface SourceRange {
  start: SourcePosition;
  end: SourcePosition;
  startByte: number;
  endByte: number;
}

export interface CallableAnchor {
  path: string;
  name: string;
  qualifiedName: string;
  signature: { normalized: string };
  nameRange: SourceRange;
  declarationRange: SourceRange;
  entityKey: string;
}

export interface CallableEntity {
  entityKey: string;
  name: string;
  qualifiedName: string;
  signature: { normalized: string };
  primaryAnchor: CallableAnchor;
}

export interface CallSiteFact {
  path: string;
  callerEntityKey: string;
  expressionRange: SourceRange;
  calleeRange: SourceRange;
  calleeName?: string;
  qualifiedName?: string;
  form: string;
  argumentCount?: number;
  siteFingerprint: string;
}

export interface EvidenceLedger {
  supports: string[];
  contradictions: string[];
  unknowns: string[];
}

export interface CallRelation {
  caller: CallableEntity;
  callee?: CallableEntity;
  direction: RelationDirection;
  callSites: CallSiteFact[];
  confidence: string;
  evidence: EvidenceLedger;
  ambiguitySetId?: string;
}

export interface CoverageSummary {
  eligibleFiles: number;
  analyzedFiles: number;
  fallbackFiles: number;
  externalBodiesLimited: boolean;
}

export interface RichRelationResponse {
  protocolVersion: number;
  revision: {
    engineEpoch: number;
    semanticGeneration: number;
    overlayEpoch: number;
    resolverVersion: number;
  };
  relations: CallRelation[];
  complete: boolean;
  budgetState: string;
  coverage: CoverageSummary;
  nextCursor?: number;
}

export function relationEntity(
  relation: CallRelation,
  direction: RelationDirection,
): CallableEntity | undefined {
  return direction === 'incoming' ? relation.caller : relation.callee;
}

export function evidenceSummary(relation: CallRelation): string {
  const groups = [
    relation.evidence.supports.length
      ? `supports: ${relation.evidence.supports.join(', ')}`
      : undefined,
    relation.evidence.contradictions.length
      ? `contradictions: ${relation.evidence.contradictions.join(', ')}`
      : undefined,
    relation.evidence.unknowns.length
      ? `unknowns: ${relation.evidence.unknowns.join(', ')}`
      : undefined,
  ].filter((value): value is string => Boolean(value));
  return groups.length ? groups.join(' · ') : 'no additional evidence';
}

export function coverageSummary(coverage: CoverageSummary): string {
  const external = coverage.externalBodiesLimited ? '; external bodies are declaration-only' : '';
  return `${coverage.analyzedFiles}/${coverage.eligibleFiles} files analyzed, ${coverage.fallbackFiles} fallback${external}`;
}
