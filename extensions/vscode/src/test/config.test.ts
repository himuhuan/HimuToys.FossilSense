import * as assert from 'assert';
import { normalizeIncludeScopingMode, normalizeOnOffAuto } from '../config';

assert.strictEqual(normalizeOnOffAuto('on'), 'on');
assert.strictEqual(normalizeOnOffAuto('off'), 'off');
assert.strictEqual(normalizeOnOffAuto('auto'), 'auto');
assert.strictEqual(normalizeOnOffAuto('unexpected'), 'auto');
assert.strictEqual(normalizeOnOffAuto(undefined), 'auto');

assert.strictEqual(normalizeIncludeScopingMode('off'), 'off');
assert.strictEqual(normalizeIncludeScopingMode('on'), 'auto');
assert.strictEqual(normalizeIncludeScopingMode(undefined), 'auto');
