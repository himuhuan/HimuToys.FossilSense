export type OnOffAutoMode = 'auto' | 'on' | 'off';
export type IncludeScopingMode = 'auto' | 'off';

export function normalizeOnOffAuto(value: string | undefined): OnOffAutoMode {
  return value === 'off' || value === 'on' ? value : 'auto';
}

export function normalizeIncludeScopingMode(value: string | undefined): IncludeScopingMode {
  return value === 'off' ? 'off' : 'auto';
}
