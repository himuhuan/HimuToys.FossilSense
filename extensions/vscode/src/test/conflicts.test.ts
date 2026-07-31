import * as assert from 'assert';
import {
  activeLanguageProviderNames,
  CONFLICT_LANGUAGE_SERVER_EXTENSIONS,
  mutualExclusionMessage,
} from '../conflicts';

const message = mutualExclusionMessage(['clangd', 'Microsoft C/C++']);

assert.ok(message.includes('clangd, Microsoft C/C++'));
assert.ok(message.includes('best-effort navigation engine'));
assert.ok(message.includes('choose one primary language provider'));
assert.ok(
  CONFLICT_LANGUAGE_SERVER_EXTENSIONS.some(
    (extension) => extension.id === 'golang.go' && extension.name.includes('gopls'),
  ),
);

const installedProviders = [
  { id: 'golang.go', isActive: false },
  { id: 'llvm-vs-code-extensions.vscode-clangd', isActive: true },
];
assert.deepStrictEqual(
  activeLanguageProviderNames(
    installedProviders.map((extension) => ({ ...extension, isActive: false })),
    new Set(['go']),
  ),
  [],
);
assert.deepStrictEqual(
  activeLanguageProviderNames(installedProviders, new Set(['c', 'cpp'])),
  ['clangd'],
);
assert.deepStrictEqual(
  activeLanguageProviderNames(
    installedProviders.map((extension) =>
      extension.id === 'golang.go' ? { ...extension, isActive: true } : extension,
    ),
    new Set(['c', 'cpp']),
  ),
  ['clangd'],
);
assert.deepStrictEqual(
  activeLanguageProviderNames(
    installedProviders.map((extension) => ({ ...extension, isActive: true })),
    new Set(['go']),
  ),
  ['Go extension (gopls)'],
);

const goMessage = mutualExclusionMessage(['Go extension (gopls)']);
assert.ok(goMessage.includes('active language-support extension'));
assert.ok(goMessage.includes('can start an overlapping language server'));
