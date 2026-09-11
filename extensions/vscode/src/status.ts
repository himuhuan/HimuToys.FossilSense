export interface DegradedCapabilities {
  reachGraph?: boolean;
  includeTable?: boolean;
  goImportTable?: boolean;
  referenceFileList?: boolean;
  projectContext?: boolean;
}

export type IndexStatusState = 'indexing' | 'deferred' | 'ready' | 'failed';

export interface WorkspaceIndexStatus {
  state: IndexStatusState;
  workspace: string;
}

const indexStatusPriority: Record<IndexStatusState, number> = {
  ready: 0,
  indexing: 1,
  deferred: 2,
  failed: 3,
};

export class IndexStatusTracker<T extends WorkspaceIndexStatus = WorkspaceIndexStatus> {
  private readonly byWorkspace = new Map<string, T>();

  update(status: T): T {
    // Reinsert the workspace so equal-priority states prefer the newest event.
    this.byWorkspace.delete(status.workspace);
    this.byWorkspace.set(status.workspace, status);
    let aggregate = status;
    for (const candidate of this.byWorkspace.values()) {
      if (indexStatusPriority[candidate.state] >= indexStatusPriority[aggregate.state]) {
        aggregate = candidate;
      }
    }
    return aggregate;
  }
}

export function degradedCapabilityWarning(degraded?: DegradedCapabilities): string | undefined {
  const labels: string[] = [];
  if (degraded?.reachGraph) {
    labels.push('reachGraph');
  }
  if (degraded?.includeTable) {
    labels.push('includeTable');
  }
  if (degraded?.goImportTable) {
    labels.push('goImportTable');
  }
  if (degraded?.referenceFileList) {
    labels.push('referenceFileList');
  }
  if (degraded?.projectContext) {
    labels.push('projectContext');
  }
  return labels.length ? labels.join(', ') : undefined;
}

export function statusTooltip(configWarning?: string, capabilityWarning?: string): string {
  const tooltipLines = ['FossilSense language server status'];
  if (configWarning) {
    tooltipLines.push(`Config warning: ${configWarning}`);
  }
  if (capabilityWarning) {
    tooltipLines.push(`Degraded capabilities: ${capabilityWarning}`);
  }
  return tooltipLines.join('\n');
}
