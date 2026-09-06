import assert from 'node:assert/strict';
import { extractClang, sourceRange } from './c_frontend_clang.mjs';
const file = '/fixtures/source.c';
const source = Buffer.from('int sample;\n');
const variable = { kind: 'VarDecl', name: 'sample', loc: { offset: 4, tokLen: 6, file },
  range: { begin: { offset: 0, tokLen: 3 }, end: { offset: 4, tokLen: 6 } } };
const ast = { kind: 'TranslationUnitDecl', inner: [
  { kind: 'TypedefDecl', name: '__builtin', isImplicit: true, loc: {} },
  { ...variable, loc: { ...variable.loc, file: '/fixtures/external.h' } },
  variable, structuredClone(variable),
] };
const result = extractClang(ast, source, file, []);
assert.equal(result.observations.length, 2, 'source multiplicity is preserved');
assert.equal(result.observations[0].kind, 'object');
assert.equal(result.observations[0].role, 'tentative_definition');
assert.deepEqual(result.observations[0].nameRange, sourceRange(source, 4, 10));
assert.deepEqual(result.observations[0].declarationRange, sourceRange(source, 0, 11));
assert.ok(result.excluded.length >= 2);
const invalid = structuredClone(variable);
invalid.loc.offset = 3;
assert.throws(() => extractClang({ inner: [invalid] }, source, file, []), /name.*source/);
assert.throws(() => extractClang({ inner: [{ ...variable, kind: 'UnexpectedDecl' }] }, source, file, []));
const unicode = Buffer.from('/* 🙂 */ int value;\r\n');
assert.deepEqual(sourceRange(unicode, 15, 20).start, { line: 0, character: 13 });
console.log('C frontend Clang adapter tests passed.');
