export type OnOffAutoMode = 'auto' | 'on' | 'off';
export type IncludeScopingMode = 'auto' | 'off';
export type ProjectContextMode = 'auto' | 'promptOnAmbiguous' | 'off';
export type CompletionPrefixRanking = 'strict' | 'scopeFirst';

export function normalizeOnOffAuto(value: string | undefined): OnOffAutoMode {
  return value === 'off' || value === 'on' ? value : 'auto';
}

export function normalizeIncludeScopingMode(value: string | undefined): IncludeScopingMode {
  return value === 'off' ? 'off' : 'auto';
}

export function normalizeProjectContextMode(value: string | undefined): ProjectContextMode {
  return value === 'promptOnAmbiguous' || value === 'off' ? value : 'auto';
}

export function normalizeCompletionPrefixRanking(
  value: string | undefined,
): CompletionPrefixRanking {
  return value === 'scopeFirst' ? 'scopeFirst' : 'strict';
}

/**
 * Normalize a boolean configuration value. Only an explicit `false` disables
 * the feature; any other value (including `undefined` from a missing setting)
 * is treated as enabled, matching the `default: true` contract.
 */
export function normalizeBoolean(value: unknown): boolean {
  return value !== false;
}

export function normalizeExternalPathList(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  const seen = new Set<string>();
  const normalized: string[] = [];
  for (const entry of value.filter((item): item is string => typeof item === 'string')) {
    const normalizedSeparators = entry.trim().replace(/\\/g, '/');
    const path =
      normalizedSeparators === '/' || /^[A-Za-z]:\/$/.test(normalizedSeparators)
        ? normalizedSeparators
        : normalizedSeparators.replace(/\/+$/, '');
    if (path.length === 0) continue;
    const key = path.toLocaleLowerCase('en-US');
    if (seen.has(key)) continue;
    seen.add(key);
    normalized.push(path);
  }
  return normalized;
}

type InspectedBoolean = Readonly<Record<string, unknown>> | undefined;

/**
 * Return the effective editor value only when a user/workspace scope actually
 * set it. The configuration schema default must not mask fossilsense.json.
 */
export function resolveExplicitBooleanOverride(
  effectiveValue: boolean,
  inspected: InspectedBoolean,
): boolean | undefined {
  if (!inspected) return undefined;
  const explicitKeys = [
    'globalValue',
    'workspaceValue',
    'workspaceFolderValue',
    'globalLanguageValue',
    'workspaceLanguageValue',
    'workspaceFolderLanguageValue',
  ];
  return explicitKeys.some((key) => inspected[key] !== undefined) ? effectiveValue : undefined;
}
