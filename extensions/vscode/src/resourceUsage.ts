/**
 * LSP notification payload sent by the Rust server every 2 seconds while it
 * is running. Field names arrive in camelCase because the Rust side annotates
 * structs with `#[serde(rename_all = "camelCase")]`. `memory` is optional so
 * an older server without per-category reporting still renders a basic
 * tooltip.
 */
export interface ProcessMemoryReport {
  totalBytes: number;
  attributedBytes: number;
  otherBytes: number;
}

export interface NameIndexMemoryReport {
  bytes: number;
  entryCount: number;
  baseSegmentBytes: number;
  deltaSegmentsBytes: number;
  deltaSegmentCount: number;
  fallbackTableBytes: number;
}

export interface DeclarationCacheMemoryReport {
  bytes: number;
  entryCount: number;
  budgetBytes: number;
  hits: number;
  misses: number;
  evictions: number;
  sqlReads: number;
}

export interface FileRelationsMemoryReport {
  bytes: number;
  reachGraphBytes: number;
  includeEdgeCount: number;
  includeTableBytes: number;
  goImportTableBytes: number;
  indexedFilesBytes: number;
  fileCount: number;
  projectContextBytes: number;
}

export interface OpenDocumentsMemoryReport {
  bytes: number;
  documentCount: number;
  overlayBytes: number;
}

export interface MemoryReport {
  process: ProcessMemoryReport;
  nameIndex: NameIndexMemoryReport;
  declarationCache: DeclarationCacheMemoryReport;
  fileRelations: FileRelationsMemoryReport;
  openDocuments: OpenDocumentsMemoryReport;
  indexDiskBytes: number;
  timestamp: number;
}

export interface ResourceUsage {
  memoryBytes: number;
  indexDiskBytes: number;
  memory?: MemoryReport;
  timestamp: number;
}

/**
 * Format a byte count as a short human-readable string suitable for the VS
 * Code status bar. Uses binary units (1 KiB = 1024 bytes) with the legacy
 * KB/MB/GB suffix to keep the label narrow. Non-finite or negative values
 * return `0B` so a corrupt payload can never render as `NaNMB`.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return '0B';
  }
  if (bytes < 1024) {
    return `${bytes}B`;
  }
  const kb = bytes / 1024;
  if (kb < 1024) {
    return `${kb.toFixed(kb < 10 ? 1 : 0)}KB`;
  }
  const mb = kb / 1024;
  if (mb < 1024) {
    return `${mb.toFixed(mb < 10 ? 1 : 0)}MB`;
  }
  const gb = mb / 1024;
  return `${gb.toFixed(gb < 10 ? 1 : 0)}GB`;
}

/** Thousands-separated count for tooltip internals (e.g. `654,321`). */
export function formatCount(value: number): string {
  if (!Number.isFinite(value) || value < 0) {
    return '0';
  }
  return Math.round(value).toLocaleString('en-US');
}

export function resourceUsageStatusText(memoryBytes: number, diskBytes: number): string {
  return `$(pulse) ${formatBytes(memoryBytes)} $(database) ${formatBytes(diskBytes)}`;
}

function shareOfTotal(bytes: number, totalBytes: number): string {
  if (!Number.isFinite(totalBytes) || totalBytes <= 0) {
    return '—';
  }
  return `${Math.round((bytes / totalBytes) * 100)}%`;
}

/**
 * Markdown source for the status bar hover. `extension.ts` wraps it in a
 * `vscode.MarkdownString`; this module stays free of the `vscode` import so
 * the unit tests can run under plain Node.
 */
export function resourceUsageTooltip(usage: ResourceUsage): string {
  if (!usage.memory) {
    return [
      'FossilSense resource usage',
      `Server memory: ${formatBytes(usage.memoryBytes)}`,
      `Index disk: ${formatBytes(usage.indexDiskBytes)}`,
      'Updated every 2 seconds while the server is running.',
    ].join('\n');
  }

  const memory = usage.memory;
  const total = memory.process.totalBytes;
  const lines = [
    '**FossilSense memory** — updated every 2 seconds',
    '',
    '| Category | Size | Share |',
    '| --- | ---: | ---: |',
    `| Process total | ${formatBytes(total)} | 100% |`,
    `| Code name index | ${formatBytes(memory.nameIndex.bytes)} | ${shareOfTotal(memory.nameIndex.bytes, total)} |`,
    `| Declaration details cache | ${formatBytes(memory.declarationCache.bytes)} | ${shareOfTotal(memory.declarationCache.bytes, total)} |`,
    `| File relations | ${formatBytes(memory.fileRelations.bytes)} | ${shareOfTotal(memory.fileRelations.bytes, total)} |`,
    `| Open editor documents | ${formatBytes(memory.openDocuments.bytes)} | ${shareOfTotal(memory.openDocuments.bytes, total)} |`,
    `| Other (runtime, allocator, older generations) | ${formatBytes(memory.process.otherBytes)} | ${shareOfTotal(memory.process.otherBytes, total)} |`,
    `| Index on disk | ${formatBytes(memory.indexDiskBytes)} | — |`,
    '',
    '**Internal stats**',
    `- Name index: ${formatCount(memory.nameIndex.entryCount)} entries · base ${formatBytes(memory.nameIndex.baseSegmentBytes)} · deltas ${formatBytes(memory.nameIndex.deltaSegmentsBytes)} (${formatCount(memory.nameIndex.deltaSegmentCount)}) · fallback ${formatBytes(memory.nameIndex.fallbackTableBytes)}`,
    `- Declaration cache: ${formatCount(memory.declarationCache.entryCount)} entries, budget ${formatBytes(memory.declarationCache.budgetBytes)} · hits ${formatCount(memory.declarationCache.hits)} · misses ${formatCount(memory.declarationCache.misses)} · evictions ${formatCount(memory.declarationCache.evictions)} · SQL reads ${formatCount(memory.declarationCache.sqlReads)}`,
    `- File relations: ${formatCount(memory.fileRelations.fileCount)} files · ${formatCount(memory.fileRelations.includeEdgeCount)} include edges · reach ${formatBytes(memory.fileRelations.reachGraphBytes)} · include ${formatBytes(memory.fileRelations.includeTableBytes)} · go imports ${formatBytes(memory.fileRelations.goImportTableBytes)} · file list ${formatBytes(memory.fileRelations.indexedFilesBytes)} · projects ${formatBytes(memory.fileRelations.projectContextBytes)}`,
    `- Open documents: ${formatCount(memory.openDocuments.documentCount)} files · overlay ${formatBytes(memory.openDocuments.overlayBytes)}`,
    '',
    'Itemized categories cover the currently published index generation of each workspace; older generations held by in-flight requests are part of "Other".',
  ];
  return lines.join('\n');
}
