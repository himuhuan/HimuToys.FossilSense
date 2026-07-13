// FossilSense VSIX packaging pipeline.
//
// One command produces a self-contained, installable .vsix in the repo's dist/:
//   1. cargo build --release          -> native engine
//   2. copy fossilsense(.exe)         -> extension bin/  (self-contained)
//   3. esbuild bundle                 -> out/extension.js (single file)
//   4. fingerprint inputs + payload   -> bin/release-build.json
//   5. vsce package --no-dependencies -> dist/fossilsense-vscode-<version>_BUILD<YYYYMMDD_HHMMSS>.vsix
//
// Run from anywhere via `pnpm run package` in extensions/vscode.

import { execFileSync, execSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
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

function captureReleaseFingerprint() {
  const hardeningScript = join(repoRoot, 'scripts', 'verify_release_hardening.ps1');
  if (!existsSync(hardeningScript)) {
    throw new Error(`Release fingerprint helper not found: ${hardeningScript}`);
  }
  const powershell = isWin ? 'powershell.exe' : 'pwsh';
  const output = execFileSync(powershell, [
    '-NoProfile',
    '-ExecutionPolicy',
    'Bypass',
    '-File',
    hardeningScript,
    '-RepoRoot',
    repoRoot,
    '-PrintReleaseFingerprint',
  ], {
    cwd: repoRoot,
    encoding: 'utf8',
    windowsHide: true,
  }).trim();
  const fingerprint = JSON.parse(output);
  if (!/^[0-9a-f]{64}$/.test(fingerprint.releaseInputSha256) ||
      !Number.isInteger(fingerprint.releaseInputFileCount) ||
      fingerprint.releaseInputFileCount <= 0) {
    throw new Error('Release fingerprint helper returned an invalid source fingerprint.');
  }
  return fingerprint;
}

function sha256File(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

function artifactPayloadFingerprint(parts) {
  const payload = [
    `releaseInput\t${parts.releaseInputSha256}`,
    `nativeBinary\t${parts.nativeBinarySha256}`,
    `extensionBundle\t${parts.extensionBundleSha256}`,
    `extensionManifest\t${parts.extensionManifestSha256}`,
    '',
  ].join('\n');
  return createHash('sha256').update(payload, 'utf8').digest('hex');
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
const stagedBinary = join(binDir, exeName);
copyFileSync(builtBinary, stagedBinary);
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

// Bind the artifact to the exact release inputs and the exact staged payload,
// including an uncommitted working tree. The manifest contains no paths or
// source text, only hashes and aggregate build provenance.
const fingerprint = captureReleaseFingerprint();
const payloadHashes = {
  releaseInputSha256: fingerprint.releaseInputSha256,
  nativeBinarySha256: sha256File(stagedBinary),
  extensionBundleSha256: sha256File(join(extDir, 'out', 'extension.js')),
  extensionManifestSha256: sha256File(join(extDir, 'package.json')),
};
const releaseBuild = {
  schemaVersion: 1,
  packageVersion: version,
  releaseInputSha256: fingerprint.releaseInputSha256,
  releaseInputFileCount: fingerprint.releaseInputFileCount,
  nativeBinarySha256: payloadHashes.nativeBinarySha256,
  extensionBundleSha256: payloadHashes.extensionBundleSha256,
  extensionManifestSha256: payloadHashes.extensionManifestSha256,
  artifactPayloadSha256: artifactPayloadFingerprint(payloadHashes),
  sourceCommit: fingerprint.sourceCommit,
  worktreeDirty: fingerprint.worktreeDirty,
};
writeFileSync(
  join(binDir, 'release-build.json'),
  `${JSON.stringify(releaseBuild, null, 2)}\n`,
  'utf8',
);
console.log(`Staged release-build.json (${fingerprint.releaseInputFileCount} inputs)`);

// 5. Package the VSIX.
const distDir = join(repoRoot, 'dist');
mkdirSync(distDir, { recursive: true });
const bts = buildTimestamp();
const vsixPath = join(distDir, `fossilsense-vscode-${version}_BUILD${bts}.vsix`);
run(NODE, [vsceBin, 'package', '--no-dependencies', '--allow-missing-repository', '-o', vsixPath], extDir);

console.log(`\n✅ VSIX ready: ${vsixPath}`);
console.log('Install: VS Code → Extensions → ... → Install from VSIX, or');
console.log(`         code --install-extension "${vsixPath}"`);
