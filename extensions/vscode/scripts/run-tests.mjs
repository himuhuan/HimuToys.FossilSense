import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const testDir = path.resolve('out/test');
const tests = fs.readdirSync(testDir)
  .filter((name) => name.endsWith('.test.js'))
  .sort();

for (const test of tests) {
  const result = spawnSync(process.execPath, [path.join(testDir, test)], { stdio: 'inherit' });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}
