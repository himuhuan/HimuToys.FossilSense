import * as assert from 'assert';
import { extensionsFromConfigText, sourceWatchGlob } from '../watchPlan';

assert.deepStrictEqual(
  extensionsFromConfigText('{"extensions":[".INO","c","ino",""]}'),
  ['c', 'ino'],
);
assert.strictEqual(sourceWatchGlob(['ino']), '**/*.ino');
assert.strictEqual(sourceWatchGlob(['c', 'ino']), '**/*.{c,ino}');
assert.strictEqual(sourceWatchGlob([]), undefined);
assert.ok(extensionsFromConfigText('{invalid').includes('cpp'));
