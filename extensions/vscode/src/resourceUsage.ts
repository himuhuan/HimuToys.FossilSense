/**
 * LSP notification payload sent by the Rust server every 5 seconds while it is
 * running. Field names arrive in camelCase because the Rust side annotates the
 * struct with `#[serde(rename_all = "camelCase")]`.
 */
export interface ResourceUsage {
  memoryBytes: number;
  indexDiskBytes: number;
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

export function resourceUsageStatusText(memoryBytes: number, diskBytes: number): string {
  return `$(pulse) ${formatBytes(memoryBytes)} $(database) ${formatBytes(diskBytes)}`;
}

export function resourceUsageTooltip(memoryBytes: number, diskBytes: number): string {
  return [
    'FossilSense resource usage',
    `Server memory: ${formatBytes(memoryBytes)}`,
    `Index disk: ${formatBytes(diskBytes)}`,
    'Updated every 5 seconds while the server is running.',
  ].join('\n');
}
