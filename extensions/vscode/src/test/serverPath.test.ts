import * as assert from 'assert';
import * as path from 'path';
import { resolveServerPathFromCandidates } from '../serverPath';

const extensionPath = path.join('repo', 'extensions', 'vscode');
const repoRoot = path.resolve(extensionPath, '..', '..');

assert.strictEqual(
  resolveServerPathFromCandidates({
    platform: 'win32',
    configuredPath: ' C:\\custom\\fossilsense.exe ',
    extensionPath,
    exists: (candidate) => candidate === 'C:\\custom\\fossilsense.exe',
  }),
  'C:\\custom\\fossilsense.exe',
);

const bundled = path.join(extensionPath, 'bin', 'fossilsense.exe');
assert.strictEqual(
  resolveServerPathFromCandidates({
    platform: 'win32',
    configuredPath: 'C:\\missing\\fossilsense.exe',
    extensionPath,
    exists: (candidate) => candidate === bundled,
  }),
  bundled,
);

const release = path.join(repoRoot, 'target', 'release', 'fossilsense.exe');
assert.strictEqual(
  resolveServerPathFromCandidates({
    platform: 'win32',
    configuredPath: '',
    extensionPath,
    exists: (candidate) => candidate === release,
  }),
  release,
);

assert.strictEqual(
  resolveServerPathFromCandidates({
    platform: 'linux',
    configuredPath: '',
    extensionPath,
    exists: () => false,
  }),
  undefined,
);
