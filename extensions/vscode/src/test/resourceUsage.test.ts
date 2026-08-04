import * as assert from 'assert';
import {
  formatBytes,
  formatCount,
  MemoryReport,
  resourceUsageStatusText,
  resourceUsageTooltip,
} from '../resourceUsage';

const MB = 1024 * 1024;

assert.strictEqual(formatBytes(0), '0B');
assert.strictEqual(formatBytes(512), '512B');
assert.strictEqual(formatBytes(1023), '1023B');
assert.strictEqual(formatBytes(1024), '1.0KB');
assert.strictEqual(formatBytes(1536), '1.5KB');
assert.strictEqual(formatBytes(12 * 1024), '12KB');
assert.strictEqual(formatBytes(1024 * 1024), '1.0MB');
assert.strictEqual(formatBytes(128 * MB), '128MB');
assert.strictEqual(formatBytes(1024 * 1024 * 1024), '1.0GB');
assert.strictEqual(formatBytes(2.5 * 1024 * 1024 * 1024), '2.5GB');
assert.strictEqual(formatBytes(-1), '0B');
assert.strictEqual(formatBytes(NaN), '0B');
assert.strictEqual(formatBytes(Infinity), '0B');

assert.strictEqual(formatCount(0), '0');
assert.strictEqual(formatCount(654321), '654,321');
assert.strictEqual(formatCount(1234), '1,234');
assert.strictEqual(formatCount(-5), '0');
assert.strictEqual(formatCount(NaN), '0');

assert.strictEqual(
  resourceUsageStatusText(128 * MB, 42 * MB),
  '$(pulse) 128MB $(database) 42MB',
);

// Legacy servers without the per-category report keep the basic tooltip.
const fallbackTooltip = resourceUsageTooltip({
  memoryBytes: 128 * MB,
  indexDiskBytes: 42 * MB,
  timestamp: 0,
});
assert.match(fallbackTooltip, /Server memory: 128MB/);
assert.match(fallbackTooltip, /Index disk: 42MB/);
assert.match(fallbackTooltip, /Updated every 2 seconds/);

const memory: MemoryReport = {
  process: {
    totalBytes: 512 * MB,
    attributedBytes: 242 * MB,
    otherBytes: 270 * MB,
  },
  nameIndex: {
    bytes: 128 * MB,
    entryCount: 654321,
    baseSegmentBytes: 100 * MB,
    deltaSegmentsBytes: 28 * MB,
    deltaSegmentCount: 3,
    fallbackTableBytes: 2 * MB,
  },
  declarationCache: {
    bytes: 64 * MB,
    entryCount: 12345,
    budgetBytes: 128 * MB,
    hits: 1234,
    misses: 56,
    evictions: 7,
    sqlReads: 8,
  },
  fileRelations: {
    bytes: 48 * MB,
    reachGraphBytes: 30 * MB,
    includeEdgeCount: 45678,
    includeTableBytes: 5 * MB,
    goImportTableBytes: 2 * MB,
    indexedFilesBytes: 10 * MB,
    fileCount: 10234,
    projectContextBytes: 1 * MB,
  },
  openDocuments: {
    bytes: 2 * MB,
    documentCount: 4,
    overlayBytes: 512 * 1024,
  },
  indexDiskBytes: 42 * MB,
  timestamp: 0,
};

const tooltip = resourceUsageTooltip({
  memoryBytes: 512 * MB,
  indexDiskBytes: 42 * MB,
  memory,
  timestamp: 0,
});

assert.match(tooltip, /\*\*FossilSense memory\*\* — updated every 2 seconds/);
assert.ok(tooltip.includes('| Process total | 512MB | 100% |'));
assert.ok(tooltip.includes('| Code name index | 128MB | 25% |'));
assert.ok(tooltip.includes('| Declaration details cache | 64MB | 13% |'));
assert.ok(tooltip.includes('| File relations | 48MB | 9% |'));
assert.ok(tooltip.includes('| Open editor documents | 2.0MB | 0% |'));
assert.ok(
  tooltip.includes('| Other (runtime, allocator, older generations) | 270MB | 53% |'),
);
assert.ok(tooltip.includes('| Index on disk | 42MB | — |'));

assert.ok(
  tooltip.includes(
    '- Name index: 654,321 entries · base 100MB · deltas 28MB (3) · fallback 2.0MB',
  ),
);
assert.ok(
  tooltip.includes(
    '- Declaration cache: 12,345 entries, budget 128MB · hits 1,234 · misses 56 · evictions 7 · SQL reads 8',
  ),
);
assert.ok(
  tooltip.includes(
    '- File relations: 10,234 files · 45,678 include edges · reach 30MB · include 5.0MB · go imports 2.0MB · file list 10MB · projects 1.0MB',
  ),
);
assert.ok(tooltip.includes('- Open documents: 4 files · overlay 512KB'));
assert.match(tooltip, /currently published index generation/);
