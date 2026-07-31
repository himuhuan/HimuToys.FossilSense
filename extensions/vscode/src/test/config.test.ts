import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import {
  normalizeBoolean,
  normalizeCompletionPrefixRanking,
  normalizeExternalPathList,
  normalizeIncludeScopingMode,
  normalizeOnOffAuto,
  normalizeProjectContextMode,
} from '../config';

assert.strictEqual(normalizeOnOffAuto('on'), 'on');
assert.strictEqual(normalizeOnOffAuto('off'), 'off');
assert.strictEqual(normalizeOnOffAuto('auto'), 'auto');
assert.strictEqual(normalizeOnOffAuto('unexpected'), 'auto');
assert.strictEqual(normalizeOnOffAuto(undefined), 'auto');

assert.strictEqual(normalizeIncludeScopingMode('off'), 'off');
assert.strictEqual(normalizeIncludeScopingMode('on'), 'auto');
assert.strictEqual(normalizeIncludeScopingMode(undefined), 'auto');

assert.strictEqual(normalizeProjectContextMode('auto'), 'auto');
assert.strictEqual(normalizeProjectContextMode('promptOnAmbiguous'), 'promptOnAmbiguous');
assert.strictEqual(normalizeProjectContextMode('off'), 'off');
assert.strictEqual(normalizeProjectContextMode('unexpected'), 'auto');

assert.strictEqual(normalizeCompletionPrefixRanking('strict'), 'strict');
assert.strictEqual(normalizeCompletionPrefixRanking('scopeFirst'), 'scopeFirst');
assert.strictEqual(normalizeCompletionPrefixRanking('unexpected'), 'strict');
assert.strictEqual(normalizeCompletionPrefixRanking(undefined), 'strict');

assert.strictEqual(normalizeBoolean(true), true);
assert.strictEqual(normalizeBoolean(false), false);
assert.strictEqual(normalizeBoolean(undefined), true);
assert.strictEqual(normalizeBoolean('unexpected'), true);
assert.strictEqual(normalizeBoolean(0), true);
assert.deepStrictEqual(
  normalizeExternalPathList([' C:\\deps\\device ', '', 7, '/opt/go/device', 'C:\\deps\\device']),
  ['C:\\deps\\device', '/opt/go/device'],
);
assert.deepStrictEqual(normalizeExternalPathList('C:\\deps'), []);

const packageJson = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'package.json'), 'utf8'),
);
const prefixRanking =
  packageJson.contributes.configuration.properties['fossilsense.completion.prefixRanking'];
assert.deepStrictEqual(prefixRanking.enum, ['strict', 'scopeFirst']);
assert.strictEqual(prefixRanking.default, 'strict');

const semanticIndexBudget =
  packageJson.contributes.configuration.properties['fossilsense.semanticIndex.memoryBudgetMB'];
assert.strictEqual(semanticIndexBudget.type, 'integer');
assert.strictEqual(semanticIndexBudget.default, 256);
assert.strictEqual(semanticIndexBudget.minimum, 0);
assert.strictEqual(semanticIndexBudget.maximum, 16384);

const resourceMonitorEnabled =
  packageJson.contributes.configuration.properties['fossilsense.resourceMonitor.enabled'];
assert.strictEqual(resourceMonitorEnabled.type, 'boolean');
assert.strictEqual(resourceMonitorEnabled.default, true);

const goModulePaths =
  packageJson.contributes.configuration.properties['fossilsense.goModulePaths'];
assert.strictEqual(goModulePaths.type, 'array');
assert.deepStrictEqual(goModulePaths.default, []);
