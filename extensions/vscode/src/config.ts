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
