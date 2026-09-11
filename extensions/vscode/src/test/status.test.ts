import * as assert from 'assert';
import {
  IndexStatusTracker,
  degradedCapabilityWarning,
  statusTooltip,
} from '../status';

assert.strictEqual(degradedCapabilityWarning(), undefined);
assert.strictEqual(degradedCapabilityWarning({ reachGraph: false }), undefined);
assert.strictEqual(
  degradedCapabilityWarning({
    reachGraph: true,
    includeTable: true,
    goImportTable: true,
    referenceFileList: true,
    projectContext: true,
  }),
  'reachGraph, includeTable, goImportTable, referenceFileList, projectContext',
);

assert.strictEqual(
  statusTooltip('bad config', 'reachGraph'),
  'FossilSense language server status\nConfig warning: bad config\nDegraded capabilities: reachGraph',
);

const statusTracker = new IndexStatusTracker();
assert.strictEqual(
  statusTracker.update({ state: 'failed', workspace: 'root-a' }).state,
  'failed',
);
assert.strictEqual(
  statusTracker.update({ state: 'ready', workspace: 'root-b' }).state,
  'failed',
  'a ready root must not hide another root that still needs recovery',
);
assert.strictEqual(
  statusTracker.update({ state: 'indexing', workspace: 'root-a' }).state,
  'indexing',
  'the stale root may enter an explicit recovery build',
);
assert.strictEqual(
  statusTracker.update({ state: 'ready', workspace: 'root-a' }).state,
  'ready',
  'all roots become ready only after the stale root recovers',
);
