export const CONFLICT_LANGUAGE_SERVER_EXTENSIONS = [
  { id: 'llvm-vs-code-extensions.vscode-clangd', name: 'clangd', languages: ['c', 'cpp'] },
  { id: 'ms-vscode.cpptools', name: 'Microsoft C/C++', languages: ['c', 'cpp'] },
  { id: 'ccls-project.ccls', name: 'ccls', languages: ['c', 'cpp'] },
  { id: 'golang.go', name: 'Go extension (gopls)', languages: ['go'] },
] as const;

interface InstalledExtensionState {
  id: string;
  isActive: boolean;
}

export function activeLanguageProviderNames(
  installedExtensions: readonly InstalledExtensionState[],
  workspaceLanguages: ReadonlySet<string>,
): string[] {
  const activeIds = new Set(
    installedExtensions
      .filter((extension) => extension.isActive)
      .map((extension) => extension.id),
  );
  return CONFLICT_LANGUAGE_SERVER_EXTENSIONS.filter(
    (extension) =>
      activeIds.has(extension.id) &&
      extension.languages.some((language) => workspaceLanguages.has(language)),
  ).map((extension) => extension.name);
}

export function mutualExclusionMessage(conflictingExtensions: string[]): string {
  const names = conflictingExtensions.join(', ');
  return (
    `FossilSense detected an active language-support extension for source languages in this workspace (${names}); it can start an overlapping language server. ` +
    'FossilSense is a best-effort navigation engine and may duplicate or disagree with it; choose one primary language provider for each language in this workspace.'
  );
}
