const DEFAULT_EXTENSIONS = ['c', 'h', 'cpp', 'hpp', 'cc', 'hh', 'cxx', 'hxx', 'inl'];

export function extensionsFromConfigText(text: string | undefined): string[] {
  if (!text) {
    return [...DEFAULT_EXTENSIONS];
  }
  try {
    const parsed = JSON.parse(text) as { extensions?: unknown };
    if (!Array.isArray(parsed.extensions)) {
      return [...DEFAULT_EXTENSIONS];
    }
    const extensions = parsed.extensions
      .filter((value): value is string => typeof value === 'string')
      .map((value) => value.trim().replace(/^\.+/, '').toLowerCase())
      .filter((value) => /^[a-z0-9_+-]+$/.test(value));
    return [...new Set(extensions)].sort();
  } catch {
    return [...DEFAULT_EXTENSIONS];
  }
}

export function sourceWatchGlob(extensions: readonly string[]): string | undefined {
  if (extensions.length === 0) {
    return undefined;
  }
  return extensions.length === 1
    ? `**/*.${extensions[0]}`
    : `**/*.{${extensions.join(',')}}`;
}
