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
  return [
    ...new Set(
      value
        .filter((entry): entry is string => typeof entry === 'string')
        .map((entry) => entry.trim())
        .filter((entry) => entry.length > 0),
    ),
  ];
}
