import { chmodSync, copyFileSync, mkdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';

export function packagePlatform(platform, arch) {
  const cpu = { x64: 'x86_64', arm64: 'aarch64' }[arch];
  const abi = { win32: 'pc-windows-msvc', linux: 'unknown-linux-gnu' }[platform];
  if (!cpu || !abi) throw new Error(`Unsupported packaging platform: ${platform}-${arch}`);
  return {
    target: `${platform}-${arch}`,
    rustTarget: `${cpu}-${abi}`,
    exeName: platform === 'win32' ? 'fossilsense.exe' : 'fossilsense',
  };
}

export function stageEngine(source, directory, { exeName }) {
  mkdirSync(directory, { recursive: true });
  const destination = join(directory, exeName);
  copyFileSync(source, destination);
  if (exeName === 'fossilsense') chmodSync(destination, 0o755);
  rmSync(join(directory, exeName === 'fossilsense' ? 'fossilsense.exe' : 'fossilsense'), { force: true });
  return destination;
}
