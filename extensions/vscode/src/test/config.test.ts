import * as assert from 'assert';
import {
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
assert.strictEqual(normalizeProjectContextMode('on'), 'auto');
assert.strictEqual(normalizeProjectContextMode(undefined), 'auto');
