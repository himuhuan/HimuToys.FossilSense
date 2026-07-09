import { ProjectContextMode } from './config';

export const SHOW_PROJECT_CONTEXT_COMMAND = 'fossilsense.showProjectContext';
export const PROJECT_CONTEXTS_LSP_COMMAND = 'fossilsense.lsp.projectContexts';
export const SET_PROJECT_CONTEXT_LSP_COMMAND = 'fossilsense.lsp.setProjectContext';
export const PROJECT_CONTEXT_WORKSPACE_STATE_KEY = 'fossilsense.projectContext.selection';

export interface ProjectContextKey {
  workspaceRootId: string;
  projectPath: string;
}

export interface ProjectContextInfo {
  key: ProjectContextKey;
  markerFiles: string[];
}

export type ProjectContextSelection =
  | { kind: 'auto' }
  | { kind: 'manual'; key: ProjectContextKey }
  | { kind: 'unspecified' };

export interface ProjectContextStatus {
  projects: ProjectContextInfo[];
  selection: ProjectContextSelection;
  automaticProject?: ProjectContextKey | null;
  activeProject?: ProjectContextKey | null;
}

export type ProjectContextPickRow =
  | { kind: 'auto'; label: string; description?: string; selection: ProjectContextSelection }
  | { kind: 'manual'; label: string; description?: string; selection: ProjectContextSelection }
  | { kind: 'unspecified'; label: string; description?: string; selection: ProjectContextSelection };

export function projectContextPickRows(projects: ProjectContextInfo[]): ProjectContextPickRow[] {
  return [
    {
      kind: 'auto',
      label: 'Current Project (Auto)',
      description: 'Nearest build marker for active file',
      selection: { kind: 'auto' },
    },
    ...projects.map((project) => ({
      kind: 'manual' as const,
      label: project.key.projectPath || '.',
      description: project.markerFiles.join(', '),
      selection: { kind: 'manual' as const, key: project.key },
    })),
    {
      kind: 'unspecified',
      label: 'Unspecified',
      description: 'Disable project-context ranking',
      selection: { kind: 'unspecified' },
    },
  ];
}

export function projectContextStatusText(
  mode: ProjectContextMode,
  status: ProjectContextStatus | undefined,
): string {
  if (mode === 'off') {
    return 'FossilSense Project: Off';
  }
  if (!status) {
    return 'FossilSense Project: ...';
  }
  if (status.selection.kind === 'unspecified') {
    return 'FossilSense Project: Unspecified';
  }
  if (status.selection.kind === 'manual') {
    return `FossilSense Project: ${displayProjectPath(status.selection.key)}`;
  }
  if (status.activeProject) {
    return `FossilSense Project: Auto ${displayProjectPath(status.activeProject)}`;
  }
  return 'FossilSense Project: Auto (none)';
}

export function validStoredProjectContextSelection(
  stored: unknown,
  projects: ProjectContextInfo[],
): ProjectContextSelection | undefined {
  if (!isProjectContextSelection(stored)) {
    return undefined;
  }
  if (stored.kind !== 'manual') {
    return stored;
  }
  return projects.some((project) => projectKeyEquals(project.key, stored.key)) ? stored : undefined;
}

export function selectionForMode(
  mode: ProjectContextMode,
  stored: ProjectContextSelection | undefined,
): ProjectContextSelection {
  if (mode === 'off') {
    return { kind: 'unspecified' };
  }
  return stored ?? { kind: 'auto' };
}

export function shouldPromptForAmbiguous(
  mode: ProjectContextMode,
  status: ProjectContextStatus | undefined,
): boolean {
  return mode === 'promptOnAmbiguous' && status?.selection.kind === 'auto' && !status.automaticProject;
}

function displayProjectPath(key: ProjectContextKey): string {
  return key.projectPath || '.';
}

function isProjectContextSelection(value: unknown): value is ProjectContextSelection {
  if (!value || typeof value !== 'object') {
    return false;
  }
  const candidate = value as Partial<ProjectContextSelection>;
  if (candidate.kind === 'auto' || candidate.kind === 'unspecified') {
    return true;
  }
  return candidate.kind === 'manual' && isProjectContextKey((candidate as { key?: unknown }).key);
}

function isProjectContextKey(value: unknown): value is ProjectContextKey {
  if (!value || typeof value !== 'object') {
    return false;
  }
  const candidate = value as Partial<ProjectContextKey>;
  return typeof candidate.workspaceRootId === 'string' && typeof candidate.projectPath === 'string';
}

function projectKeyEquals(left: ProjectContextKey, right: ProjectContextKey): boolean {
  return left.workspaceRootId === right.workspaceRootId && left.projectPath === right.projectPath;
}
