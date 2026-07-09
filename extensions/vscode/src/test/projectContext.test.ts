import * as assert from 'assert';
import {
  ProjectContextInfo,
  projectContextPickRows,
  projectContextStatusText,
  selectionForMode,
  shouldPromptForAmbiguous,
  validStoredProjectContextSelection,
} from '../projectContext';

const projects: ProjectContextInfo[] = [
  {
    key: { workspaceRootId: 'root-a', projectPath: 'app' },
    markerFiles: ['Makefile'],
  },
  {
    key: { workspaceRootId: 'root-b', projectPath: 'third_party/lib' },
    markerFiles: ['CMakeLists.txt'],
  },
];

const rows = projectContextPickRows(projects);
assert.deepStrictEqual(
  rows.map((row) => row.label),
  ['Current Project (Auto)', 'app', 'third_party/lib', 'Unspecified'],
);
assert.deepStrictEqual(rows[1].selection, {
  kind: 'manual',
  key: { workspaceRootId: 'root-a', projectPath: 'app' },
});

assert.deepStrictEqual(
  validStoredProjectContextSelection(
    { kind: 'manual', key: { workspaceRootId: 'root-a', projectPath: 'app' } },
    projects,
  ),
  { kind: 'manual', key: { workspaceRootId: 'root-a', projectPath: 'app' } },
);
assert.strictEqual(
  validStoredProjectContextSelection(
    { kind: 'manual', key: { workspaceRootId: 'root-a', projectPath: 'deleted' } },
    projects,
  ),
  undefined,
);
assert.strictEqual(
  validStoredProjectContextSelection(
    { kind: 'manual', key: { workspace_root_id: 'root-a', project_path: 'app' } },
    projects,
  ),
  undefined,
);

assert.deepStrictEqual(selectionForMode('off', rows[1].selection), { kind: 'unspecified' });
assert.deepStrictEqual(selectionForMode('auto', undefined), { kind: 'auto' });

assert.strictEqual(
  projectContextStatusText('auto', {
    projects,
    selection: { kind: 'auto' },
    automaticProject: projects[0].key,
    activeProject: projects[0].key,
  }),
  'FossilSense Project: Auto app',
);
assert.strictEqual(
  projectContextStatusText('auto', {
    projects,
    selection: { kind: 'unspecified' },
    automaticProject: undefined,
    activeProject: undefined,
  }),
  'FossilSense Project: Unspecified',
);
assert.strictEqual(projectContextStatusText('off', undefined), 'FossilSense Project: Off');

assert.strictEqual(
  shouldPromptForAmbiguous('promptOnAmbiguous', {
    projects,
    selection: { kind: 'auto' },
    automaticProject: undefined,
    activeProject: undefined,
  }),
  true,
);
assert.strictEqual(
  shouldPromptForAmbiguous('promptOnAmbiguous', {
    projects,
    selection: { kind: 'auto' },
    automaticProject: projects[0].key,
    activeProject: projects[0].key,
  }),
  false,
);
