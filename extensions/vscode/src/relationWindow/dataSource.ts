import {
  EvidenceRelationData,
  EvidenceRelationParams,
  ExpandRelationData,
  ExpandRelationParams,
  LocateRelationData,
  LocateRelationParams,
  PrepareRelationData,
  PrepareRelationParams,
  RelationInvalidationReason,
  RelationResult,
} from '../callRelationsModel';

export interface RelationDisposable {
  dispose(): void;
}

export interface RelationDataSource {
  prepare(
    params: PrepareRelationParams,
    signal: AbortSignal,
  ): Promise<RelationResult<PrepareRelationData>>;
  expand(
    params: ExpandRelationParams,
    signal: AbortSignal,
  ): Promise<RelationResult<ExpandRelationData>>;
  evidence(
    params: EvidenceRelationParams,
    signal: AbortSignal,
  ): Promise<RelationResult<EvidenceRelationData>>;
  locate(
    params: LocateRelationParams,
    signal: AbortSignal,
  ): Promise<RelationResult<LocateRelationData>>;
  onInvalidated(listener: (reason: RelationInvalidationReason) => void): RelationDisposable;
  dispose(): void;
}
