import * as assert from 'assert';
import { degradedCapabilityWarning, statusTooltip } from '../status';

assert.strictEqual(degradedCapabilityWarning(), undefined);
assert.strictEqual(degradedCapabilityWarning({ reachGraph: false }), undefined);
assert.strictEqual(
  degradedCapabilityWarning({
    reachGraph: true,
    includeTable: true,
    referenceFileList: true,
  }),
  'reachGraph, includeTable, referenceFileList',
);

assert.strictEqual(
  statusTooltip('bad config', 'reachGraph'),
  'FossilSense language server status\nConfig warning: bad config\nDegraded capabilities: reachGraph',
);
