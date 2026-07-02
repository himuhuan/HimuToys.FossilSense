export function mutualExclusionMessage(conflictingExtensions: string[]): string {
  const names = conflictingExtensions.join(', ');
  return (
    `FossilSense detected another C/C++ language server (${names}). ` +
    'FossilSense is a best-effort navigation engine and may duplicate or disagree with it; choose one primary C/C++ provider for this workspace.'
  );
}
