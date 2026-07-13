import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import {
  normalizeCompletionPrefixRanking,
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

const packageJson = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'package.json'), 'utf8'),
);
const prefixRanking =
  packageJson.contributes.configuration.properties['fossilsense.completion.prefixRanking'];
assert.deepStrictEqual(prefixRanking.enum, ['strict', 'scopeFirst']);
assert.strictEqual(prefixRanking.default, 'strict');
