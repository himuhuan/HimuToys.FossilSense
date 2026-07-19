import * as assert from 'assert';
import {
  formatBytes,
  resourceUsageStatusText,
  resourceUsageTooltip,
} from '../resourceUsage';

assert.strictEqual(formatBytes(0), '0B');
assert.strictEqual(formatBytes(512), '512B');
assert.strictEqual(formatBytes(1023), '1023B');
assert.strictEqual(formatBytes(1024), '1.0KB');
assert.strictEqual(formatBytes(1536), '1.5KB');
assert.strictEqual(formatBytes(12 * 1024), '12KB');
assert.strictEqual(formatBytes(1024 * 1024), '1.0MB');
assert.strictEqual(formatBytes(128 * 1024 * 1024), '128MB');
assert.strictEqual(formatBytes(1024 * 1024 * 1024), '1.0GB');
assert.strictEqual(formatBytes(2.5 * 1024 * 1024 * 1024), '2.5GB');
assert.strictEqual(formatBytes(-1), '0B');
assert.strictEqual(formatBytes(NaN), '0B');
assert.strictEqual(formatBytes(Infinity), '0B');

assert.strictEqual(
  resourceUsageStatusText(128 * 1024 * 1024, 42 * 1024 * 1024),
  '$(pulse) 128MB $(database) 42MB',
);

const tooltip = resourceUsageTooltip(128 * 1024 * 1024, 42 * 1024 * 1024);
assert.match(tooltip, /Server memory: 128MB/);
assert.match(tooltip, /Index disk: 42MB/);
assert.match(tooltip, /Updated every 5 seconds/);
