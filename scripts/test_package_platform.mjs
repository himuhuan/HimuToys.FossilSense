import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, existsSync, statSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { packagePlatform, stageEngine } from '../extensions/vscode/scripts/package-platform.mjs';
assert.deepEqual(packagePlatform('linux', 'x64'), { target: 'linux-x64', rustTarget: 'x86_64-unknown-linux-gnu', exeName: 'fossilsense' });
assert.equal(packagePlatform('win32', 'arm64').target, 'win32-arm64');
assert.throws(() => packagePlatform('linux', 'ia32'), /Unsupported/);
const dir = mkdtempSync(join(tmpdir(), 'fossilsense-package-'));
try {
  const source = join(dir, 'engine');
  writeFileSync(source, 'native engine', { mode: 0o644 });
  writeFileSync(join(dir, 'fossilsense.exe'), 'stale Windows engine');
  const staged = stageEngine(source, dir, packagePlatform('linux', 'x64'));
  assert.equal(readFileSync(staged, 'utf8'), 'native engine');
  assert.equal(existsSync(join(dir, 'fossilsense.exe')), false);
  if (process.platform !== 'win32') assert.ok(statSync(staged).mode & 0o100);
} finally { rmSync(dir, { recursive: true, force: true }); }
