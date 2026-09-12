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
  semanticGeneration: number;
  incompleteReason?: string;
}

export interface CompactCallRelation {
  callerId: number;
  calleeId?: number;
  direction: RelationDirection;
  callSites: CallSiteFact[];
  confidence: string;
  evidence: EvidenceLedger;
  ambiguitySetId?: string;
}

export interface RichRelationWireResponse {
  protocolVersion: number;
  revision: {
    engineEpoch: number;
    semanticGeneration: number;
    overlayEpoch: number;
    resolverVersion: number;
  };
  entities: Record<string, CallableEntity>;
  relations: CompactCallRelation[];
  complete: boolean;
  budgetState: string;
  coverage: CoverageSummary;
  nextCursor?: string;
}

export interface RichRelationResponse extends Omit<RichRelationWireResponse, 'relations'> {
  relations: CallRelation[];
}

export function normalizeRichRelationResponse(
  response: RichRelationWireResponse,
): RichRelationResponse {
  if (response.protocolVersion !== 2) {
    throw new Error(`unsupported call relation protocol ${response.protocolVersion}; expected 2`);
  }
  const relations = response.relations.map((relation): CallRelation => {
    const caller = response.entities[String(relation.callerId)];
    const callee =
      relation.calleeId === undefined
        ? undefined
        : response.entities[String(relation.calleeId)];
    if (!caller || (relation.calleeId !== undefined && !callee)) {
      throw new Error('call relation response references a missing entity dictionary entry');
    }
    return {
      caller,
      callee,
      direction: relation.direction,
      callSites: relation.callSites,
      confidence: relation.confidence,
      evidence: relation.evidence,
      ambiguitySetId: relation.ambiguitySetId,
    };
  });
  return { ...response, relations };
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

// Frozen format-3 target contract. The format-2 normalizer above remains the
// active call-tree path until the backend migration switches all consumers.
export const RELATION_WINDOW_PROTOCOL_VERSION = 3;
export const RELATION_REQUEST_MAX_BYTES = 32 * 1024;
export const RELATION_RESPONSE_MAX_BYTES = 1024 * 1024;

export type RelationOperation = 'prepare' | 'expand' | 'evidence' | 'locate';
export type RelationFamily = 'c_cpp' | 'go';
export type RelationKind =
  | 'calls'
  | 'references'
  | 'members'
  | 'inherits'
  | 'typeOf'
  | 'includes';
export type RelationNodeKind =
  | 'callable'
  | 'record'
  | 'field'
  | 'variable'
  | 'local'
  | 'alias'
  | 'macro'
  | 'file';
export type RelationSupport = 'supported' | 'partial' | 'unsupported';
export type RelationResolution = 'resolved' | 'candidate' | 'unresolved';
export type RelationRole =
  | 'read'
  | 'write'
  | 'read_write'
  | 'call'
  | 'type_use'
  | 'address'
  | 'unknown';
export type RelationCoverageState = 'complete' | 'partial' | 'unavailable';
export type RelationBudget =
  | 'within'
  | 'scan_limit'
  | 'candidate_limit'
  | 'response_limit'
  | 'deadline';
export type RelationErrorCode =
  | 'version_mismatch'
  | 'invalid_request'
  | 'invalid_reference'
  | 'invalid_cursor'
  | 'stale_snapshot'
  | 'snapshot_unavailable'
  | 'unsupported'
  | 'cancelled'
  | 'resource_limit'
  | 'execution_failed';
export type RelationInvalidationReason =
  | 'document_changed'
  | 'index_published'
  | 'configuration_changed'
  | 'service_restarted'
  | 'service_stopped';

export interface RelationSnapshot {
  serviceInstance: string;
  databaseIdentity: string;
  workspaceUri: string;
  family: RelationFamily;
  engineEpoch: string;
  semanticGeneration: string;
  overlayEpoch: string;
  resolverVersion: number;
}

export interface RelationPosition {
  line: number;
  character: number;
}

export interface RelationRange {
  start: RelationPosition;
  end: RelationPosition;
}

export interface RelationLocation {
  uri: string;
  range: RelationRange;
  documentVersion: number | null;
}

export type RelationRootInput =
  | {
      kind: 'symbol';
      uri: string;
      position: RelationPosition;
      documentVersion: number | null;
    }
  | { kind: 'file'; uri: string };

export interface RelationCapability {
  relation: RelationKind;
  direction: RelationDirection;
  support: RelationSupport;
  reason: string | null;
}

export interface RelationNode {
  ref: string;
  kind: RelationNodeKind;
  name: string;
  qualifiedName: string;
  detail: string | null;
  displayTruncated: boolean;
  relations: RelationCapability[];
}

export interface KnownSiteCount {
  value: number;
  exact: boolean;
}

export interface RelationEdge {
  ref: string;
  sourceRef: string;
  targetRef: string | null;
  relation: RelationKind;
  resolution: RelationResolution;
  ambiguitySetRef: string | null;
  roles: RelationRole[];
  knownSiteCount: KnownSiteCount;
  reasonCodes: string[];
}

export interface RelationEvidenceItem {
  ref: string;
  location: RelationLocation | null;
  excerpt: string | null;
  excerptTruncated: boolean;
  supports: string[];
  contradictions: string[];
  unknowns: string[];
}

export interface RelationCoverage {
  state: RelationCoverageState;
  scope: string;
  reasons: string[];
}

export type RelationContinuation =
  | { kind: 'end' }
  | { kind: 'page'; cursor: string }
  | { kind: 'limited'; reason: string };

export interface RelationPageMeta {
  continuation: RelationContinuation;
  coverage: RelationCoverage;
  budget: RelationBudget;
}

export interface PrepareRelationParams {
  root: RelationRootInput;
}

export interface ExpandRelationParams {
  snapshot: RelationSnapshot;
  nodeRef: string;
  relation: RelationKind;
  direction: RelationDirection;
  cursor: string | null;
  pageSize: number;
}

export interface EvidenceRelationParams {
  snapshot: RelationSnapshot;
  edgeRef: string;
  cursor: string | null;
  pageSize: number;
}

export type RelationLocateTarget =
  | { kind: 'node'; ref: string }
  | { kind: 'evidence'; ref: string };
export type RelationLocateIntent = 'definition' | 'declaration' | 'site';

export interface LocateRelationParams {
  snapshot: RelationSnapshot;
  target: RelationLocateTarget;
  intent: RelationLocateIntent;
  cursor: string | null;
  pageSize: number;
}

export type RelationRequest =
  | { protocolVersion: 3; operation: 'prepare'; params: PrepareRelationParams }
  | { protocolVersion: 3; operation: 'expand'; params: ExpandRelationParams }
  | { protocolVersion: 3; operation: 'evidence'; params: EvidenceRelationParams }
  | { protocolVersion: 3; operation: 'locate'; params: LocateRelationParams };

export interface PrepareRelationData {
  snapshot: RelationSnapshot;
  roots: RelationNode[];
  selectionRequired: boolean;
  coverage: RelationCoverage;
}

export interface ExpandRelationData {
  snapshot: RelationSnapshot;
  nodes: RelationNode[];
  edges: RelationEdge[];
  page: RelationPageMeta;
}

export interface EvidenceRelationData {
  snapshot: RelationSnapshot;
  items: RelationEvidenceItem[];
  page: RelationPageMeta;
}

export interface LocateRelationData {
  snapshot: RelationSnapshot;
  locations: RelationLocation[];
  page: RelationPageMeta;
}

export interface RelationDataByOperation {
  prepare: PrepareRelationData;
  expand: ExpandRelationData;
  evidence: EvidenceRelationData;
  locate: LocateRelationData;
}

export interface RelationWireError {
  code: RelationErrorCode;
  message: string;
  retryable: boolean;
  reasonCodes: string[];
}

export type RelationResult<T> =
  | { protocolVersion: 3; outcome: 'ok'; data: T }
  | { protocolVersion: 3; outcome: 'error'; error: RelationWireError };

export class RelationContractValidationError extends Error {
  constructor(
    public readonly code: RelationErrorCode,
    message: string,
  ) {
    super(message);
    this.name = 'RelationContractValidationError';
  }
}

type JsonObject = Record<string, unknown>;

const relationKinds: readonly RelationKind[] = [
  'calls',
  'references',
  'members',
  'inherits',
  'typeOf',
  'includes',
];
const relationNodeKinds: readonly RelationNodeKind[] = [
  'callable',
  'record',
  'field',
  'variable',
  'local',
  'alias',
  'macro',
  'file',
];
const relationRoles: readonly RelationRole[] = [
  'read',
  'write',
  'read_write',
  'call',
  'type_use',
  'address',
  'unknown',
];
const relationErrorCodes: readonly RelationErrorCode[] = [
  'version_mismatch',
  'invalid_request',
  'invalid_reference',
  'invalid_cursor',
  'stale_snapshot',
  'snapshot_unavailable',
  'unsupported',
  'cancelled',
  'resource_limit',
  'execution_failed',
];

export function normalizeRelationRequest(value: unknown): RelationRequest {
  enforceWireBytes(value, RELATION_REQUEST_MAX_BYTES, 'request');
  const request = objectValue(value, 'request');
  exactKeys(request, ['protocolVersion', 'operation', 'params'], 'request');
  validateProtocol(request.protocolVersion);
  const operation = enumValue(
    request.operation,
    ['prepare', 'expand', 'evidence', 'locate'] as const,
    'request.operation',
  );
  const params = objectValue(request.params, 'request.params');
  switch (operation) {
    case 'prepare':
      exactKeys(params, ['root'], 'prepare.params');
      validateRoot(params.root, 'prepare.params.root');
      break;
    case 'expand':
      exactKeys(
        params,
        ['snapshot', 'nodeRef', 'relation', 'direction', 'cursor', 'pageSize'],
        'expand.params',
      );
      validateSnapshot(params.snapshot, 'expand.params.snapshot');
      limitedString(params.nodeRef, 4096, 'expand.params.nodeRef', true);
      enumValue(params.relation, relationKinds, 'expand.params.relation');
      enumValue(params.direction, ['incoming', 'outgoing'] as const, 'expand.params.direction');
      nullableString(params.cursor, 4096, 'expand.params.cursor', true);
      pageSize(params.pageSize, 'expand.params.pageSize');
      break;
    case 'evidence':
      exactKeys(params, ['snapshot', 'edgeRef', 'cursor', 'pageSize'], 'evidence.params');
      validateSnapshot(params.snapshot, 'evidence.params.snapshot');
      limitedString(params.edgeRef, 4096, 'evidence.params.edgeRef', true);
      nullableString(params.cursor, 4096, 'evidence.params.cursor', true);
      pageSize(params.pageSize, 'evidence.params.pageSize');
      break;
    case 'locate':
      exactKeys(
        params,
        ['snapshot', 'target', 'intent', 'cursor', 'pageSize'],
        'locate.params',
      );
      validateSnapshot(params.snapshot, 'locate.params.snapshot');
      validateLocateTarget(params.target, params.intent, 'locate.params');
      nullableString(params.cursor, 4096, 'locate.params.cursor', true);
      pageSize(params.pageSize, 'locate.params.pageSize');
      break;
  }
  return value as RelationRequest;
}

export function normalizeRelationResult<O extends RelationOperation>(
  operation: O,
  value: unknown,
): RelationResult<RelationDataByOperation[O]> {
  enforceWireBytes(value, RELATION_RESPONSE_MAX_BYTES, 'response');
  const result = objectValue(value, 'response');
  const outcome = enumValue(result.outcome, ['ok', 'error'] as const, 'response.outcome');
  if (outcome === 'ok') {
    exactKeys(result, ['protocolVersion', 'outcome', 'data'], 'response');
  } else {
    exactKeys(result, ['protocolVersion', 'outcome', 'error'], 'response');
  }
  validateProtocol(result.protocolVersion);
  if (outcome === 'error') {
    validateWireError(result.error, 'response.error');
    return value as RelationResult<RelationDataByOperation[O]>;
  }
  switch (operation) {
    case 'prepare':
      validatePrepareData(result.data, 'response.data');
      break;
    case 'expand':
      validateExpandData(result.data, 'response.data');
      break;
    case 'evidence':
      validateEvidenceData(result.data, 'response.data');
      break;
    case 'locate':
      validateLocateData(result.data, 'response.data');
      break;
  }
  return value as RelationResult<RelationDataByOperation[O]>;
}

export function mergeKnownSiteCount(
  current: KnownSiteCount,
  incoming: KnownSiteCount,
): KnownSiteCount {
  validateKnownSiteCount(current, 'current');
  validateKnownSiteCount(incoming, 'incoming');
  if (current.exact && incoming.exact && current.value !== incoming.value) {
    invalid('conflicting exact knownSiteCount values');
  }
  if (
    (current.exact && !incoming.exact && incoming.value > current.value) ||
    (incoming.exact && !current.exact && current.value > incoming.value)
  ) {
    invalid('knownSiteCount lower bound exceeds exact value');
  }
  if (current.exact) {
    return current;
  }
  if (incoming.exact) {
    return incoming;
  }
  return { value: Math.max(current.value, incoming.value), exact: false };
}

export function sameRelationSnapshot(left: RelationSnapshot, right: RelationSnapshot): boolean {
  return (
    left.serviceInstance === right.serviceInstance &&
    left.databaseIdentity === right.databaseIdentity &&
    left.workspaceUri === right.workspaceUri &&
    left.family === right.family &&
    left.engineEpoch === right.engineEpoch &&
    left.semanticGeneration === right.semanticGeneration &&
    left.overlayEpoch === right.overlayEpoch &&
    left.resolverVersion === right.resolverVersion
  );
}

function validatePrepareData(value: unknown, path: string): void {
  const data = objectValue(value, path);
  exactKeys(data, ['snapshot', 'roots', 'selectionRequired', 'coverage'], path);
  validateSnapshot(data.snapshot, `${path}.snapshot`);
  const roots = arrayValue(data.roots, `${path}.roots`);
  if (roots.length > 200) invalid(`${path}.roots exceeds 200 items`);
  roots.forEach((node, index) => validateNode(node, `${path}.roots[${index}]`));
  booleanValue(data.selectionRequired, `${path}.selectionRequired`);
  validateCoverage(data.coverage, `${path}.coverage`);
}

function validateExpandData(value: unknown, path: string): void {
  const data = objectValue(value, path);
  exactKeys(data, ['snapshot', 'nodes', 'edges', 'page'], path);
  validateSnapshot(data.snapshot, `${path}.snapshot`);
  const nodes = arrayValue(data.nodes, `${path}.nodes`);
  const edges = arrayValue(data.edges, `${path}.edges`);
  if (nodes.length > 400) invalid(`${path}.nodes exceeds 400 items`);
  if (edges.length > 200) invalid(`${path}.edges exceeds 200 items`);
  const nodeRefs = new Set<string>();
  nodes.forEach((node, index) => {
    validateNode(node, `${path}.nodes[${index}]`);
    const reference = (node as JsonObject).ref as string;
    if (nodeRefs.has(reference)) invalid(`${path}.nodes contains duplicate ref ${reference}`);
    nodeRefs.add(reference);
  });
  edges.forEach((edge, index) => {
    validateEdge(edge, `${path}.edges[${index}]`);
    const wire = edge as JsonObject;
    if (!nodeRefs.has(wire.sourceRef as string)) {
      invalidReference(`${path}.edges[${index}].sourceRef is absent from nodes`);
    }
    if (wire.targetRef !== null && !nodeRefs.has(wire.targetRef as string)) {
      invalidReference(`${path}.edges[${index}].targetRef is absent from nodes`);
    }
  });
  validatePage(data.page, `${path}.page`);
}

function validateEvidenceData(value: unknown, path: string): void {
  const data = objectValue(value, path);
  exactKeys(data, ['snapshot', 'items', 'page'], path);
  validateSnapshot(data.snapshot, `${path}.snapshot`);
  const items = arrayValue(data.items, `${path}.items`);
  if (items.length > 200) invalid(`${path}.items exceeds 200 items`);
  items.forEach((item, index) => validateEvidenceItem(item, `${path}.items[${index}]`));
  validatePage(data.page, `${path}.page`);
}

function validateLocateData(value: unknown, path: string): void {
  const data = objectValue(value, path);
  exactKeys(data, ['snapshot', 'locations', 'page'], path);
  validateSnapshot(data.snapshot, `${path}.snapshot`);
  const locations = arrayValue(data.locations, `${path}.locations`);
  if (locations.length > 200) invalid(`${path}.locations exceeds 200 items`);
  locations.forEach((location, index) => validateLocation(location, `${path}.locations[${index}]`));
  validatePage(data.page, `${path}.page`);
}

function validateRoot(value: unknown, path: string): void {
  const root = objectValue(value, path);
  const kind = enumValue(root.kind, ['symbol', 'file'] as const, `${path}.kind`);
  if (kind === 'symbol') {
    exactKeys(root, ['kind', 'uri', 'position', 'documentVersion'], path);
    limitedString(root.uri, 8192, `${path}.uri`, true);
    validatePosition(root.position, `${path}.position`);
    nullableSafeInteger(root.documentVersion, `${path}.documentVersion`);
  } else {
    exactKeys(root, ['kind', 'uri'], path);
    limitedString(root.uri, 8192, `${path}.uri`, true);
  }
}

function validateSnapshot(value: unknown, path: string): void {
  const snapshot = objectValue(value, path);
  exactKeys(
    snapshot,
    [
      'serviceInstance',
      'databaseIdentity',
      'workspaceUri',
      'family',
      'engineEpoch',
      'semanticGeneration',
      'overlayEpoch',
      'resolverVersion',
    ],
    path,
  );
  limitedString(snapshot.serviceInstance, 4096, `${path}.serviceInstance`, true);
  limitedString(snapshot.databaseIdentity, 4096, `${path}.databaseIdentity`, true);
  limitedString(snapshot.workspaceUri, 8192, `${path}.workspaceUri`, true);
  enumValue(snapshot.family, ['c_cpp', 'go'] as const, `${path}.family`);
  decimalU64(snapshot.engineEpoch, `${path}.engineEpoch`);
  decimalU64(snapshot.semanticGeneration, `${path}.semanticGeneration`);
  decimalU64(snapshot.overlayEpoch, `${path}.overlayEpoch`);
  safeUnsignedInteger(snapshot.resolverVersion, `${path}.resolverVersion`);
}

function validateNode(value: unknown, path: string): void {
  const node = objectValue(value, path);
  exactKeys(
    node,
    ['ref', 'kind', 'name', 'qualifiedName', 'detail', 'displayTruncated', 'relations'],
    path,
  );
  limitedString(node.ref, 4096, `${path}.ref`, true);
  enumValue(node.kind, relationNodeKinds, `${path}.kind`);
  limitedString(node.name, 512, `${path}.name`, true);
  limitedString(node.qualifiedName, 1024, `${path}.qualifiedName`);
  nullableString(node.detail, 2048, `${path}.detail`);
  booleanValue(node.displayTruncated, `${path}.displayTruncated`);
  arrayValue(node.relations, `${path}.relations`).forEach((capability, index) =>
    validateCapability(capability, `${path}.relations[${index}]`),
  );
}

function validateCapability(value: unknown, path: string): void {
  const capability = objectValue(value, path);
  exactKeys(capability, ['relation', 'direction', 'support', 'reason'], path);
  enumValue(capability.relation, relationKinds, `${path}.relation`);
  enumValue(capability.direction, ['incoming', 'outgoing'] as const, `${path}.direction`);
  enumValue(capability.support, ['supported', 'partial', 'unsupported'] as const, `${path}.support`);
  nullableString(capability.reason, 512, `${path}.reason`);
}

function validateEdge(value: unknown, path: string): void {
  const edge = objectValue(value, path);
  exactKeys(
    edge,
    [
      'ref',
      'sourceRef',
      'targetRef',
      'relation',
      'resolution',
      'ambiguitySetRef',
      'roles',
      'knownSiteCount',
      'reasonCodes',
    ],
    path,
  );
  limitedString(edge.ref, 4096, `${path}.ref`, true);
  limitedString(edge.sourceRef, 4096, `${path}.sourceRef`, true);
  nullableString(edge.targetRef, 4096, `${path}.targetRef`, true);
  enumValue(edge.relation, relationKinds, `${path}.relation`);
  const resolution = enumValue(
    edge.resolution,
    ['resolved', 'candidate', 'unresolved'] as const,
    `${path}.resolution`,
  );
  if ((resolution === 'unresolved') !== (edge.targetRef === null)) {
    invalid(`${path}.targetRef does not match resolution`);
  }
  nullableString(edge.ambiguitySetRef, 4096, `${path}.ambiguitySetRef`, true);
  arrayValue(edge.roles, `${path}.roles`).forEach((role, index) =>
    enumValue(role, relationRoles, `${path}.roles[${index}]`),
  );
  validateKnownSiteCount(edge.knownSiteCount, `${path}.knownSiteCount`);
  reasonList(edge.reasonCodes, `${path}.reasonCodes`);
}

function validateKnownSiteCount(value: unknown, path: string): void {
  const count = objectValue(value, path);
  exactKeys(count, ['value', 'exact'], path);
  safeUnsignedInteger(count.value, `${path}.value`);
  booleanValue(count.exact, `${path}.exact`);
}

function validateEvidenceItem(value: unknown, path: string): void {
  const item = objectValue(value, path);
  exactKeys(
    item,
    ['ref', 'location', 'excerpt', 'excerptTruncated', 'supports', 'contradictions', 'unknowns'],
    path,
  );
  limitedString(item.ref, 4096, `${path}.ref`, true);
  if (item.location !== null) validateLocation(item.location, `${path}.location`);
  nullableString(item.excerpt, 4096, `${path}.excerpt`);
  booleanValue(item.excerptTruncated, `${path}.excerptTruncated`);
  reasonList(item.supports, `${path}.supports`);
  reasonList(item.contradictions, `${path}.contradictions`);
  reasonList(item.unknowns, `${path}.unknowns`);
}

function validateLocation(value: unknown, path: string): void {
  const location = objectValue(value, path);
  exactKeys(location, ['uri', 'range', 'documentVersion'], path);
  limitedString(location.uri, 8192, `${path}.uri`, true);
  validateRange(location.range, `${path}.range`);
  nullableSafeInteger(location.documentVersion, `${path}.documentVersion`);
}

function validateRange(value: unknown, path: string): void {
  const range = objectValue(value, path);
  exactKeys(range, ['start', 'end'], path);
  const start = validatePosition(range.start, `${path}.start`);
  const end = validatePosition(range.end, `${path}.end`);
  if (end.line < start.line || (end.line === start.line && end.character < start.character)) {
    invalid(`${path}.end precedes start`);
  }
}

function validatePosition(value: unknown, path: string): RelationPosition {
  const position = objectValue(value, path);
  exactKeys(position, ['line', 'character'], path);
  safeUnsignedInteger(position.line, `${path}.line`);
  safeUnsignedInteger(position.character, `${path}.character`);
  return position as unknown as RelationPosition;
}

function validateCoverage(value: unknown, path: string): void {
  const coverage = objectValue(value, path);
  exactKeys(coverage, ['state', 'scope', 'reasons'], path);
  enumValue(coverage.state, ['complete', 'partial', 'unavailable'] as const, `${path}.state`);
  limitedString(coverage.scope, 512, `${path}.scope`);
  reasonList(coverage.reasons, `${path}.reasons`);
}

function validatePage(value: unknown, path: string): void {
  const page = objectValue(value, path);
  exactKeys(page, ['continuation', 'coverage', 'budget'], path);
  const continuation = objectValue(page.continuation, `${path}.continuation`);
  const kind = enumValue(
    continuation.kind,
    ['end', 'page', 'limited'] as const,
    `${path}.continuation.kind`,
  );
  if (kind === 'end') exactKeys(continuation, ['kind'], `${path}.continuation`);
  if (kind === 'page') {
    exactKeys(continuation, ['kind', 'cursor'], `${path}.continuation`);
    limitedString(continuation.cursor, 4096, `${path}.continuation.cursor`, true);
  }
  if (kind === 'limited') {
    exactKeys(continuation, ['kind', 'reason'], `${path}.continuation`);
    limitedString(continuation.reason, 512, `${path}.continuation.reason`, true);
  }
  validateCoverage(page.coverage, `${path}.coverage`);
  enumValue(
    page.budget,
    ['within', 'scan_limit', 'candidate_limit', 'response_limit', 'deadline'] as const,
    `${path}.budget`,
  );
}

function validateLocateTarget(targetValue: unknown, intentValue: unknown, path: string): void {
  const target = objectValue(targetValue, `${path}.target`);
  exactKeys(target, ['kind', 'ref'], `${path}.target`);
  const kind = enumValue(target.kind, ['node', 'evidence'] as const, `${path}.target.kind`);
  limitedString(target.ref, 4096, `${path}.target.ref`, true);
  const intent = enumValue(
    intentValue,
    ['definition', 'declaration', 'site'] as const,
    `${path}.intent`,
  );
  if ((kind === 'node' && intent === 'site') || (kind === 'evidence' && intent !== 'site')) {
    invalid(`${path}.target kind and intent are incompatible`);
  }
}

function validateWireError(value: unknown, path: string): void {
  const error = objectValue(value, path);
  exactKeys(error, ['code', 'message', 'retryable', 'reasonCodes'], path);
  enumValue(error.code, relationErrorCodes, `${path}.code`);
  limitedString(error.message, 512, `${path}.message`, true);
  booleanValue(error.retryable, `${path}.retryable`);
  reasonList(error.reasonCodes, `${path}.reasonCodes`);
}

function validateProtocol(value: unknown): void {
  if (!Number.isInteger(value)) invalid('protocolVersion must be an integer');
  if (value !== RELATION_WINDOW_PROTOCOL_VERSION) {
    throw new RelationContractValidationError(
      'version_mismatch',
      `unsupported relation protocol ${String(value)}; expected ${RELATION_WINDOW_PROTOCOL_VERSION}`,
    );
  }
}

function objectValue(value: unknown, path: string): JsonObject {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    invalid(`${path} must be an object`);
  }
  return value as JsonObject;
}

function arrayValue(value: unknown, path: string): unknown[] {
  if (!Array.isArray(value)) invalid(`${path} must be an array`);
  return value;
}

function exactKeys(value: JsonObject, expected: readonly string[], path: string): void {
  const expectedSet = new Set(expected);
  for (const key of Object.keys(value)) {
    if (!expectedSet.has(key)) invalid(`${path} contains unknown field ${key}`);
  }
  for (const key of expected) {
    if (!Object.prototype.hasOwnProperty.call(value, key)) invalid(`${path} is missing ${key}`);
  }
}

function enumValue<T extends string>(value: unknown, allowed: readonly T[], path: string): T {
  if (typeof value !== 'string' || !allowed.includes(value as T)) {
    invalid(`${path} has an unknown value`);
  }
  return value as T;
}

function booleanValue(value: unknown, path: string): boolean {
  if (typeof value !== 'boolean') invalid(`${path} must be boolean`);
  return value;
}

function safeUnsignedInteger(value: unknown, path: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
    invalid(`${path} must be a non-negative JavaScript safe integer`);
  }
  return value;
}

function nullableSafeInteger(value: unknown, path: string): void {
  if (value !== null && (typeof value !== 'number' || !Number.isSafeInteger(value))) {
    invalid(`${path} must be a JavaScript safe integer or null`);
  }
}

function pageSize(value: unknown, path: string): void {
  const size = safeUnsignedInteger(value, path);
  if (size < 1 || size > 200) invalid(`${path} must be between 1 and 200`);
}

function decimalU64(value: unknown, path: string): void {
  if (typeof value !== 'string' || !/^(0|[1-9][0-9]*)$/.test(value)) {
    invalid(`${path} must be a canonical decimal u64 string`);
  }
  try {
    if (BigInt(value) > 18446744073709551615n) invalid(`${path} exceeds u64`);
  } catch {
    invalid(`${path} must be a canonical decimal u64 string`);
  }
}

function nullableString(
  value: unknown,
  maximum: number,
  path: string,
  nonempty = false,
): void {
  if (value !== null) limitedString(value, maximum, path, nonempty);
}

function limitedString(value: unknown, maximum: number, path: string, nonempty = false): string {
  if (typeof value !== 'string') invalid(`${path} must be a string`);
  if (nonempty && value.length === 0) invalid(`${path} must not be empty`);
  if (Array.from(value).length > maximum) invalid(`${path} exceeds ${maximum} Unicode code points`);
  return value;
}

function reasonList(value: unknown, path: string): void {
  const values = arrayValue(value, path);
  if (values.length > 32) invalid(`${path} exceeds 32 entries`);
  values.forEach((reason, index) => limitedString(reason, 512, `${path}[${index}]`));
}

function enforceWireBytes(value: unknown, maximum: number, label: string): void {
  let json: string;
  try {
    json = JSON.stringify(value);
  } catch {
    invalid(`${label} is not JSON serializable`);
  }
  if (new TextEncoder().encode(json!).byteLength > maximum) {
    invalid(`${label} exceeds ${maximum} UTF-8 bytes`);
  }
}

function invalid(message: string): never {
  throw new RelationContractValidationError('invalid_request', message);
}

function invalidReference(message: string): never {
  throw new RelationContractValidationError('invalid_reference', message);
}
