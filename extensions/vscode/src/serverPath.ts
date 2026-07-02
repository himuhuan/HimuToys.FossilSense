import * as path from 'path';

export interface ServerPathInputs {
  platform: NodeJS.Platform;
  configuredPath: string;
  extensionPath: string;
  exists: (candidate: string) => boolean;
}

export function resolveServerPathFromCandidates(inputs: ServerPathInputs): string | undefined {
  const exeName = inputs.platform === 'win32' ? 'fossilsense.exe' : 'fossilsense';

  const configured = inputs.configuredPath.trim();
  if (configured && inputs.exists(configured)) {
    return configured;
  }

  const bundled = path.join(inputs.extensionPath, 'bin', exeName);
  if (inputs.exists(bundled)) {
    return bundled;
  }

  const repoRoot = path.resolve(inputs.extensionPath, '..', '..');
  for (const profile of ['release', 'debug']) {
    const candidate = path.join(repoRoot, 'target', profile, exeName);
    if (inputs.exists(candidate)) {
      return candidate;
    }
  }

  return undefined;
}
