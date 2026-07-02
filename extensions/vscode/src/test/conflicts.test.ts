import * as assert from 'assert';
import { mutualExclusionMessage } from '../conflicts';

const message = mutualExclusionMessage(['clangd', 'Microsoft C/C++']);

assert.ok(message.includes('clangd, Microsoft C/C++'));
assert.ok(message.includes('best-effort navigation engine'));
assert.ok(message.includes('choose one primary C/C++ provider'));
