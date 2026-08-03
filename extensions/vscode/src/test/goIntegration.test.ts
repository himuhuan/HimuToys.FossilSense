import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import {
  PROJECT_CONTEXT_MARKER_PATTERNS,
  SUPPORTED_LANGUAGE_IDS,
  languageDocumentSelectors,
  isSupportedLocalDocument,
} from '../languageSupport';

assert.deepStrictEqual(SUPPORTED_LANGUAGE_IDS, ['c', 'cpp', 'go']);
assert.deepStrictEqual(languageDocumentSelectors(), [
  { scheme: 'file', language: 'c' },
  { scheme: 'file', language: 'cpp' },
  { scheme: 'file', language: 'go' },
]);
assert.ok(PROJECT_CONTEXT_MARKER_PATTERNS.includes('**/go.mod'));
assert.ok(PROJECT_CONTEXT_MARKER_PATTERNS.includes('**/go.work'));
assert.ok(isSupportedLocalDocument('file', 'go'));
assert.ok(!isSupportedLocalDocument('untitled', 'go'));

const packageJson = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'package.json'), 'utf8'),
);
assert.strictEqual(packageJson.version, '1.5.2');
assert.ok(packageJson.description.includes('Go'));
assert.ok(packageJson.activationEvents.includes('onLanguage:go'));
for (const item of packageJson.contributes.menus['editor/context']) {
  assert.ok(item.when.includes('editorLangId == go'));
}

const extensionSource = fs.readFileSync(
  path.join(__dirname, '..', '..', 'src', 'extension.ts'),
  'utf8',
);
assert.ok(
  extensionSource.includes("event.affectsConfiguration('fossilsense.goModulePaths')"),
  'changing client-forwarded Go module roots must restart the language server',
);
assert.ok(
  !extensionSource.includes('await detectedLanguageServers()'),
  'conflict advice must not create an await window before the language client startup guard',
);

const extensionConfigSource = fs.readFileSync(
  path.join(__dirname, '..', '..', 'src', 'extensionConfig.ts'),
  'utf8',
);
assert.ok(
  !extensionConfigSource.includes('workspace.findFiles'),
  'conflict advice must not scan a large workspace on the server startup path',
);
assert.ok(
  extensionConfigSource.includes('export function detectedLanguageServers(): string[]'),
  'conflict advice must stay synchronous so concurrent Start calls remain serialized',
);
