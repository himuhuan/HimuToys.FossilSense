import { ProjectContextMode } from './config';

export const SELECT_PROJECT_CONTEXT_COMMAND = 'fossilsense.selectProjectContext';
export const PROJECT_CONTEXTS_LSP_COMMAND = 'fossilsense.lsp.projectContexts';
export const SET_PROJECT_CONTEXT_LSP_COMMAND = 'fossilsense.lsp.setProjectContext';
export const PROJECT_CONTEXT_WORKSPACE_STATE_KEY = 'fossilsense.projectContext.selection';

export interface ProjectKey {
  workspaceRootId: string;
  projectPath: string;
}

export interface ProjectContextInfo {
  key: ProjectKey;
  workspaceName: string;
  markerFiles: string[];
}

export type ProjectContextSelection =
  | { kind: 'auto' }
  | { kind: 'manual'; key: ProjectKey }
  | { kind: 'unspecified' };

export interface ProjectContextStatus {
  available: boolean;
  projects: ProjectContextInfo[];
  selection: ProjectContextSelection;
  automaticProject?: ProjectKey | null;
  activeProject?: ProjectKey | null;
}

export interface ProjectContextPickRow {
  label: string;
  description: string;
  selection: ProjectContextSelection;
}

export function projectContextPickRows(projects: ProjectContextInfo[]): ProjectContextPickRow[] {
  return [
    {
      label: 'Current Project (Auto)',
      description: 'Use the active file\'s nearest build marker',
      selection: { kind: 'auto' },
    },
    {
      label: 'Unspecified',
      description: 'Use baseline completion without project evidence',
      selection: { kind: 'unspecified' },
    },
    ...projects.map((project) => {
      return {
        label: projectDisplayLabel(project, projects),
        description: `${project.workspaceName} · ${project.markerFiles.join(', ')}`,
        selection: { kind: 'manual' as const, key: project.key },
      };
    }),
  ];
}

export function projectContextStatusText(
  mode: ProjectContextMode,
  status: ProjectContextStatus | undefined,
): string {
  if (mode === 'off') {
    return '$(circle-slash) Project: Off';
  }
  if (!status) {
    return '$(folder) Project: …';
  }
  if (!status.available) {
    return '$(warning) Project: unavailable';
  }
  if (status.selection.kind === 'unspecified') {
    return '$(circle-slash) Project: Unspecified';
  }
  if (status.selection.kind === 'manual') {
    return `$(folder) ${projectKeyDisplayLabel(status.selection.key, status.projects)}`;
  }
  return status.activeProject
    ? `$(folder) Auto: ${projectKeyDisplayLabel(status.activeProject, status.projects)}`
    : '$(folder) Auto: none';
}

export function projectContextTooltip(
  mode: ProjectContextMode,
  status: ProjectContextStatus | undefined,
): string {
  const lines = [
    'FossilSense project context',
    'Build markers are best-effort ordinary-completion ranking evidence only.',
    'They do not filter candidates or change navigation, references, coloring, members, or includes.',
  ];
  if (mode === 'off') {
    lines.push('Mode: off (baseline completion)');
    return lines.join('\n');
  }
  if (!status) {
    lines.push('State: waiting for the language server');
    return lines.join('\n');
  }
  if (!status.available) {
    lines.push('State: project model unavailable; baseline completion is active');
    return lines.join('\n');
  }
  lines.push(`Selection: ${selectionLabel(status.selection)}`);
  if (status.automaticProject) {
    lines.push(`Automatic: ${displayPath(status.automaticProject.projectPath)}`);
  }
  if (status.activeProject) {
    const info = status.projects.find((project) => projectKeyEquals(project.key, status.activeProject!));
    lines.push(`Effective: ${projectKeyDisplayLabel(status.activeProject, status.projects)}`);
    if (info) {
      lines.push(`Workspace: ${info.workspaceName}`);
      lines.push(`Markers: ${info.markerFiles.join(', ')}`);
    }
  }
  return lines.join('\n');
}

export function validStoredProjectContextSelection(
  stored: unknown,
  projects: ProjectContextInfo[],
): ProjectContextSelection | undefined {
  if (!isSelection(stored)) {
    return undefined;
  }
  if (stored.kind !== 'manual') {
    return stored;
  }
  const project = projects.find((candidate) => projectKeyEquals(candidate.key, stored.key));
  return project ? { kind: 'manual', key: project.key } : undefined;
}

export function effectiveSelectionForMode(
  mode: ProjectContextMode,
  stored: ProjectContextSelection | undefined,
): ProjectContextSelection {
  return mode === 'off' ? { kind: 'unspecified' } : stored ?? { kind: 'auto' };
}

export function shouldPromptForProjectContext(
  mode: ProjectContextMode,
  status: ProjectContextStatus | undefined,
): boolean {
  return Boolean(
    mode === 'promptOnAmbiguous' &&
      status?.available &&
      status.projects.length > 0 &&
      status.selection.kind === 'auto' &&
      !status.automaticProject,
  );
}

export class ProjectContextPromptTracker {
  private readonly prompted = new Set<string>();

  claim(uri: string): boolean {
    if (this.prompted.has(uri)) {
      return false;
    }
    this.prompted.add(uri);
    return true;
  }

  clear(): void {
    this.prompted.clear();
  }
}

function selectionLabel(selection: ProjectContextSelection): string {
  switch (selection.kind) {
    case 'auto':
      return 'Current Project (Auto)';
    case 'unspecified':
      return 'Unspecified';
    case 'manual':
      return displayPath(selection.key.projectPath);
  }
}

function displayPath(path: string): string {
  return path || '.';
}

function projectKeyDisplayLabel(key: ProjectKey, projects: ProjectContextInfo[]): string {
  const project = projects.find((candidate) => projectKeyEquals(candidate.key, key));
  return project ? projectDisplayLabel(project, projects) : displayPath(key.projectPath);
}

function projectDisplayLabel(
  project: ProjectContextInfo,
  projects: ProjectContextInfo[],
): string {
  const path = displayPath(project.key.projectPath);
  const samePath = projects.filter(
    (candidate) =>
      displayPath(candidate.key.projectPath).toLowerCase() === path.toLowerCase(),
  );
  if (samePath.length <= 1) {
    return path;
  }

  const base = `${project.workspaceName} · ${path}`;
  const sameWorkspaceAndPath = samePath.filter(
    (candidate) => candidate.workspaceName.toLowerCase() === project.workspaceName.toLowerCase(),
  );
  return sameWorkspaceAndPath.length > 1
    ? `${base} [${project.key.workspaceRootId}]`
    : base;
}

function isSelection(value: unknown): value is ProjectContextSelection {
  if (!value || typeof value !== 'object') {
    return false;
  }
  const selection = value as { kind?: unknown; key?: unknown };
  if (selection.kind === 'auto' || selection.kind === 'unspecified') {
    return true;
  }
  return selection.kind === 'manual' && isProjectKey(selection.key);
}

function isProjectKey(value: unknown): value is ProjectKey {
  if (!value || typeof value !== 'object') {
    return false;
  }
  const key = value as Partial<ProjectKey>;
  return typeof key.workspaceRootId === 'string' && typeof key.projectPath === 'string';
}

function projectKeyEquals(left: ProjectKey, right: ProjectKey): boolean {
  return (
    left.workspaceRootId === right.workspaceRootId &&
    left.projectPath.toLowerCase() === right.projectPath.toLowerCase()
  );
}
