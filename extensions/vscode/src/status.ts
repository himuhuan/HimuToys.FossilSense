export interface DegradedCapabilities {
  reachGraph?: boolean;
  includeTable?: boolean;
  referenceFileList?: boolean;
  projectContext?: boolean;
}

export function degradedCapabilityWarning(degraded?: DegradedCapabilities): string | undefined {
  const labels: string[] = [];
  if (degraded?.reachGraph) {
    labels.push('reachGraph');
  }
  if (degraded?.includeTable) {
    labels.push('includeTable');
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
