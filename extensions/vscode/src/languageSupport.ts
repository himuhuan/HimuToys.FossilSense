export const SUPPORTED_LANGUAGE_IDS = ['c', 'cpp', 'go'] as const;

export const PROJECT_CONTEXT_MARKER_PATTERNS = [
  '**/Makefile',
  '**/GNUmakefile',
  '**/CMakeLists.txt',
  '**/*.pro',
  '**/build.ninja',
  '**/*.sln',
  '**/*.vcxproj',
  '**/*.vcproj',
  '**/meson.build',
  '**/BUILD',
  '**/BUILD.bazel',
  '**/WORKSPACE',
  '**/WORKSPACE.bazel',
  '**/go.mod',
  '**/go.work',
] as const;

export function languageDocumentSelectors(): Array<{ scheme: string; language: string }> {
  return SUPPORTED_LANGUAGE_IDS.map((language) => ({ scheme: 'file', language }));
}

export function isSupportedLocalDocument(scheme: string, languageId: string): boolean {
  return (
    scheme === 'file' &&
    SUPPORTED_LANGUAGE_IDS.some((supported) => supported === languageId)
  );
}
