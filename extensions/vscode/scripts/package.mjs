// FossilSense VSIX packaging pipeline.
//
// One command produces a self-contained, installable .vsix in the repo's dist/:
//   1. cargo build --release          -> native engine
//   2. copy fossilsense(.exe)         -> extension bin/  (self-contained)
//   3. esbuild bundle                 -> out/extension.js (single file)
//   4. vsce package --no-dependencies -> dist/fossilsense-vscode-<version>_BUILD<YYYYMMDD_HHMMSS>.vsix
//
// Run from anywhere via `pnpm run package` in extensions/vscode.

import { execFileSync, execSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const extDir = resolve(fileURLToPath(import.meta.url), '..', '..'); // extensions/vscode
const repoRoot = resolve(extDir, '..', '..');
const isWin = process.platform === 'win32';
const exeName = isWin ? 'fossilsense.exe' : 'fossilsense';

function buildTimestamp() {
  const d = new Date();
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}_${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
}

const pkg = JSON.parse(readFileSync(join(extDir, 'package.json'), 'utf8'));
const { version } = pkg;

// Local node-based CLIs, invoked as `node <script>` for cross-platform robustness
// (avoids relying on node_modules/.bin shims being on PATH).
const NODE = process.execPath;
const esbuildBin = join(extDir, 'node_modules', 'esbuild', 'bin', 'esbuild');
const vsceBin = join(extDir, 'node_modules', '@vscode', 'vsce', 'vsce');

// Full-path node invocations: no shell (a space in C:\Program Files\... would be
// split into a bogus command), args passed as an array.
function run(file, args, cwd) {
  console.log(`\n> ${file} ${args.join(' ')}`);
  execFileSync(file, args, { cwd, stdio: 'inherit' });
}

// Bare command resolved via PATH (cargo). A command string + shell avoids both the
// PATH-lookup problem and the DEP0190 warning from mixing an args array with a shell.
function runShell(command, cwd) {
  console.log(`\n> ${command}`);
  execSync(command, { cwd, stdio: 'inherit' });
}

function requireDep(path, name) {
  if (!existsSync(path)) {
    throw new Error(`Missing dev dependency "${name}". Run \`pnpm install\` in ${extDir} first.`);
  }
}

requireDep(esbuildBin, 'esbuild');
requireDep(vsceBin, '@vscode/vsce');

// 1. Build the native engine in release mode.
runShell('cargo build --release -p fossilsense', repoRoot);

// 2. Stage the binary inside the extension so the VSIX is self-contained.
const builtBinary = join(repoRoot, 'target', 'release', exeName);
if (!existsSync(builtBinary)) {
  throw new Error(`Expected release binary not found: ${builtBinary}`);
}
const binDir = join(extDir, 'bin');
mkdirSync(binDir, { recursive: true });
copyFileSync(builtBinary, join(binDir, exeName));
console.log(`Staged ${exeName} -> ${binDir}`);

// 3. Bundle the TypeScript client into a single file (sidesteps pnpm symlinks for vsce).
run(NODE, [
  esbuildBin,
  'src/extension.ts',
  '--bundle',
  '--outfile=out/extension.js',
  '--external:vscode',
  '--format=cjs',
  '--platform=node',
  '--target=node18',
], extDir);

// 4. Package the VSIX.
const distDir = join(repoRoot, 'dist');
mkdirSync(distDir, { recursive: true });
const bts = buildTimestamp();
const vsixPath = join(distDir, `fossilsense-vscode-${version}_BUILD${bts}.vsix`);
run(NODE, [vsceBin, 'package', '--no-dependencies', '--allow-missing-repository', '-o', vsixPath], extDir);

console.log(`\n✅ VSIX ready: ${vsixPath}`);
console.log('Install: VS Code → Extensions → ... → Install from VSIX, or');
console.log(`         code --install-extension "${vsixPath}"`);
