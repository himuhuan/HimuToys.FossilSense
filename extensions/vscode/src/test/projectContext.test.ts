import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import {
  ProjectContextInfo,
  ProjectContextPromptTracker,
  effectiveSelectionForMode,
  projectContextPickRows,
  projectContextStatusText,
  projectContextTooltip,
  shouldPromptForProjectContext,
  validStoredProjectContextSelection,
} from '../projectContext';

const projects: ProjectContextInfo[] = [
  {
    key: { workspaceRootId: 'root-a', projectPath: 'src/server' },
    workspaceName: 'firmware',
    markerFiles: ['Makefile'],
  },
  {
    key: { workspaceRootId: 'root-b', projectPath: 'src/server' },
    workspaceName: 'sdk',
    markerFiles: ['CMakeLists.txt'],
  },
  {
    key: { workspaceRootId: 'root-a', projectPath: '' },
    workspaceName: 'firmware',
    markerFiles: ['WORKSPACE.bazel'],
  },
];

const rows = projectContextPickRows(projects);
assert.deepStrictEqual(
  rows.map((row) => row.label),
  ['Current Project (Auto)', 'Unspecified', 'firmware · src/server', 'sdk · src/server', '.'],
);
assert.match(rows[2].description, /firmware.*Makefile/);
assert.deepStrictEqual(rows[4].selection, {
  kind: 'manual',
  key: { workspaceRootId: 'root-a', projectPath: '' },
});

const sameNamedWorkspaceProjects: ProjectContextInfo[] = [
  projects[0],
  {
    key: { workspaceRootId: 'root-c', projectPath: 'src/server' },
    workspaceName: 'firmware',
    markerFiles: ['meson.build'],
  },
];
assert.deepStrictEqual(
  projectContextPickRows(sameNamedWorkspaceProjects).map((row) => row.label),
  [
    'Current Project (Auto)',
    'Unspecified',
    'firmware · src/server [root-a]',
    'firmware · src/server [root-c]',
  ],
);

const manual = {
  kind: 'manual' as const,
  key: { workspaceRootId: 'root-a', projectPath: 'SRC/SERVER' },
};
assert.deepStrictEqual(validStoredProjectContextSelection(manual, projects), {
  kind: 'manual',
  key: projects[0].key,
});
assert.strictEqual(
  validStoredProjectContextSelection(
    { kind: 'manual', key: { workspaceRootId: 'root-a', projectPath: 'deleted' } },
    projects,
  ),
  undefined,
);
assert.strictEqual(validStoredProjectContextSelection({ kind: 'broken' }, projects), undefined);

assert.deepStrictEqual(effectiveSelectionForMode('off', manual), { kind: 'unspecified' });
assert.deepStrictEqual(effectiveSelectionForMode('auto', manual), manual);
assert.deepStrictEqual(effectiveSelectionForMode('auto', undefined), { kind: 'auto' });

const resolvedStatus = {
  available: true,
  projects,
  selection: { kind: 'auto' as const },
  automaticProject: projects[0].key,
  activeProject: projects[0].key,
};
assert.strictEqual(
  projectContextStatusText('auto', resolvedStatus),
  '$(folder) Auto: firmware · src/server',
);
assert.match(projectContextTooltip('auto', resolvedStatus), /Markers: Makefile/);
assert.strictEqual(
  projectContextStatusText('auto', {
    ...resolvedStatus,
    projects: sameNamedWorkspaceProjects,
  }),
  '$(folder) Auto: firmware · src/server [root-a]',
);
assert.strictEqual(projectContextStatusText('off', resolvedStatus), '$(circle-slash) Project: Off');
assert.strictEqual(
  projectContextStatusText('auto', { ...resolvedStatus, available: false }),
  '$(warning) Project: unavailable',
);
assert.strictEqual(
  projectContextStatusText('auto', {
    ...resolvedStatus,
    selection: { kind: 'unspecified' },
    automaticProject: undefined,
    activeProject: undefined,
  }),
  '$(circle-slash) Project: Unspecified',
);

const unresolvedStatus = {
  available: true,
  projects,
  selection: { kind: 'auto' as const },
  automaticProject: undefined,
  activeProject: undefined,
};
assert.strictEqual(shouldPromptForProjectContext('promptOnAmbiguous', unresolvedStatus), true);
assert.strictEqual(shouldPromptForProjectContext('auto', unresolvedStatus), false);
assert.strictEqual(
  shouldPromptForProjectContext('promptOnAmbiguous', { ...unresolvedStatus, projects: [] }),
  false,
);
assert.strictEqual(
  shouldPromptForProjectContext('promptOnAmbiguous', { ...unresolvedStatus, available: false }),
  false,
);

const tracker = new ProjectContextPromptTracker();
assert.strictEqual(tracker.claim('file:///a.c'), true);
assert.strictEqual(tracker.claim('file:///a.c'), false);
assert.strictEqual(tracker.claim('file:///b.c'), true);
tracker.clear();
assert.strictEqual(tracker.claim('file:///a.c'), true);

const packageJson = JSON.parse(
  fs.readFileSync(path.resolve(__dirname, '../../package.json'), 'utf8'),
);
assert.ok(
  packageJson.contributes.commands.some(
    (entry: { command?: string }) => entry.command === 'fossilsense.selectProjectContext',
  ),
);
assert.deepStrictEqual(
  packageJson.contributes.configuration.properties['fossilsense.projectContext.mode'].enum,
  ['auto', 'promptOnAmbiguous', 'off'],
);
