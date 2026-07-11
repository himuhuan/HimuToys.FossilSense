import * as fs from 'fs';
import * as vscode from 'vscode';
import {
  ExecuteCommandRequest,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  Trace,
} from 'vscode-languageclient/node';
import {
  normalizeIncludeScopingMode,
  normalizeOnOffAuto,
  normalizeProjectContextMode,
} from './config';
import {
  CLEAR_COMPLETION_HISTORY_COMMAND,
  clearCompletionHistoryRequest,
  completionHistoryInitializationOptions,
} from './completionHistory';
import { resolveServerPathFromCandidates } from './serverPath';
import {
  DegradedCapabilities,
  degradedCapabilityWarning,
  statusTooltip,
} from './status';
import { mutualExclusionMessage } from './conflicts';
import { GroupedReferenceItem, groupedReferencePickRows } from './referencesView';
import {
  CallRelationsController,
  registerCallRelationViews,
} from './callRelationsView';
import {
  PROJECT_CONTEXTS_LSP_COMMAND,
  PROJECT_CONTEXT_WORKSPACE_STATE_KEY,
  ProjectContextPromptTracker,
  ProjectContextSelection,
  ProjectContextStatus,
  SELECT_PROJECT_CONTEXT_COMMAND,
  SET_PROJECT_CONTEXT_LSP_COMMAND,
  effectiveSelectionForMode,
  projectContextPickRows,
  projectContextStatusText,
  projectContextTooltip,
  shouldPromptForProjectContext,
  validStoredProjectContextSelection,
} from './projectContext';

const REFRESH_INDEX_COMMAND = 'fossilsense.refreshIndex';
const REFRESH_INDEX_LSP_COMMAND = 'fossilsense.lsp.refreshIndex';
const REBUILD_INDEX_COMMAND = 'fossilsense.rebuildIndex';
const REBUILD_INDEX_LSP_COMMAND = 'fossilsense.lsp.rebuildIndex';
const GROUPED_REFERENCES_COMMAND = 'fossilsense.findReferencesGrouped';
const GROUPED_REFERENCES_LSP_COMMAND = 'fossilsense.lsp.groupedReferences';
const PROJECT_CONTEXT_MARKER_PATTERNS = [
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
];

const CONFLICT_EXTENSIONS = [
  { id: 'llvm-vs-code-extensions.vscode-clangd', name: 'clangd' },
  { id: 'ms-vscode.cpptools', name: 'Microsoft C/C++' },
  { id: 'ccls-project.ccls', name: 'ccls' },
];

let client: LanguageClient | undefined;
let statusBar: vscode.StatusBarItem;
let projectContextStatusBar: vscode.StatusBarItem;
let callRelationsController: CallRelationsController;
let output: vscode.OutputChannel;
let configWarning: string | undefined;
let capabilityWarning: string | undefined;
let currentIndexStartedWithWarning = false;
let mutualExclusionWarningShown = false;
const projectContextPromptTracker = new ProjectContextPromptTracker();
let projectContextUpdateEpoch = 0;

interface IndexStatus {
  state: 'indexing' | 'ready' | 'failed';
  workspace: string;
  phase?: string;
  processedFiles: number;
  totalFiles: number;
  indexedFiles: number;
  skippedFiles: number;
  symbols: number;
  semanticGeneration: number;
  elapsedMs: number;
  discoverMs: number;
  parseMs: number;
  writeMs: number;
  checkMs: number;
  includeEdgeMs: number;
  nameTableMs: number;
  reachGraphMs: number;
  degradedCapabilities?: DegradedCapabilities;
  message?: string;
}

export function activate(context: vscode.ExtensionContext): void {
  output = vscode.window.createOutputChannel('FossilSense');
  statusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  statusBar.command = 'fossilsense.startServer';
  setStatus('stopped');
  statusBar.show();
  projectContextStatusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 99);
  projectContextStatusBar.command = SELECT_PROJECT_CONTEXT_COMMAND;
  setProjectContextStatus(undefined);
  projectContextStatusBar.show();
  callRelationsController = registerCallRelationViews(context, () => client);

  context.subscriptions.push(
    output,
    statusBar,
    projectContextStatusBar,
    vscode.commands.registerCommand('fossilsense.startServer', () => startServer(context)),
    vscode.commands.registerCommand('fossilsense.stopServer', () => stopServer()),
    vscode.commands.registerCommand(REFRESH_INDEX_COMMAND, () => refreshIndex()),
    vscode.commands.registerCommand(REBUILD_INDEX_COMMAND, () => rebuildIndex()),
    vscode.commands.registerCommand(GROUPED_REFERENCES_COMMAND, () => findReferencesGrouped()),
    vscode.commands.registerCommand(CLEAR_COMPLETION_HISTORY_COMMAND, () =>
      clearCompletionHistory(),
    ),
    vscode.commands.registerCommand(SELECT_PROJECT_CONTEXT_COMMAND, () =>
      showProjectContextSelector(context, false),
    ),
    vscode.window.onDidChangeActiveTextEditor(() =>
      updateProjectContextForActiveEditor(context),
    ),
    // These settings are sent via initializationOptions or control startup, so
    // changing them requires a restart to take effect.
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration('fossilsense.mode')) {
        output.appendLine('fossilsense.mode changed; restarting server.');
        await stopServer();
        await startServer(context);
        return;
      }
      if (event.affectsConfiguration('fossilsense.projectContext.mode') && client) {
        await applyProjectContextSelectionFromState(context);
        await updateProjectContextForActiveEditor(context);
        return;
      }
      if (
        client &&
        (event.affectsConfiguration('fossilsense.includePaths') ||
          event.affectsConfiguration('fossilsense.completion.mode') ||
          event.affectsConfiguration('fossilsense.completionHistory.mode') ||
          event.affectsConfiguration('fossilsense.semanticColoring.mode') ||
          event.affectsConfiguration('fossilsense.includeScoping.mode') ||
          event.affectsConfiguration('fossilsense.debug.candidateReasons') ||
          event.affectsConfiguration('fossilsense.trace.server'))
      ) {
        output.appendLine('FossilSense configuration changed; restarting server.');
        await stopServer();
        await startServer(context);
      }
    }),
  );

  // Auto-start when a workspace is open; the manual command stays as a fallback.
  if (vscode.workspace.workspaceFolders?.length) {
    void startServer(context);
  }
}

export async function deactivate(): Promise<void> {
  await stopServer();
}

async function startServer(context: vscode.ExtensionContext): Promise<void> {
  if (client) {
    output.appendLine('FossilSense server is already running.');
    return;
  }

  const fossilsenseMode = fossilsenseModeFromConfig();
  if (fossilsenseMode === 'off') {
    setStatus('disabled');
    output.appendLine('FossilSense is disabled by fossilsense.mode=off.');
    void vscode.window.showInformationMessage(
      'FossilSense is disabled by fossilsense.mode=off. Change the setting to start it.',
    );
    return;
  }

  const workspaceFolders = vscode.workspace.workspaceFolders;
  const firstWorkspaceFolder = workspaceFolders?.[0];
  if (!firstWorkspaceFolder) {
    void vscode.window.showWarningMessage('Open a workspace folder before starting FossilSense.');
    return;
  }

  const serverPath = resolveServerPath(context);
  if (!serverPath) {
    setStatus('scan failed');
    void vscode.window.showErrorMessage(
      'FossilSense server binary was not found. Run `cargo build` or set `fossilsense.serverPath`.',
    );
    return;
  }

  setStatus('starting');
  output.appendLine(`Starting FossilSense server: ${serverPath}`);
  output.appendLine(
    `Workspaces: ${workspaceFolders.map((folder) => folder.uri.fsPath).join('; ')}`,
  );

  const serverOptions: ServerOptions = {
    command: serverPath,
    args: ['lsp'],
    options: {
      cwd: firstWorkspaceFolder.uri.fsPath,
    },
  };

  const fileEvents = [
    vscode.workspace.createFileSystemWatcher('**/*.{c,h,cpp,hpp,cc,hh,cxx,hxx,inl}'),
    ...workspaceFolders.map((folder) =>
      vscode.workspace.createFileSystemWatcher(
        new vscode.RelativePattern(folder, 'fossilsense.json'),
      ),
    ),
    ...workspaceFolders.flatMap((folder) =>
      PROJECT_CONTEXT_MARKER_PATTERNS.map((pattern) =>
        vscode.workspace.createFileSystemWatcher(new vscode.RelativePattern(folder, pattern)),
      ),
    ),
  ];

  const conflictingExtensions = detectedCppLanguageServers();

  const completionMode = completionModeFromConfig();
  const completionHistoryMode = completionHistoryModeFromConfig();
  const semanticColoringMode = semanticColoringModeFromConfig();

  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: 'file', language: 'c' },
      { scheme: 'file', language: 'cpp' },
    ],
    outputChannel: output,
    synchronize: {
      fileEvents,
    },
    initializationOptions: {
      fossilsense: {
        completion: {
          mode: completionMode,
        },
        ...completionHistoryInitializationOptions(completionHistoryMode),
        semanticColoring: {
          mode: semanticColoringMode,
        },
        includeScoping: {
          mode: includeScopingModeFromConfig(),
        },
        projectContext: {
          mode: projectContextModeFromConfig(),
        },
        includePaths: includePathsFromConfig(),
        debug: {
          candidateReasons: debugCandidateReasonsFromConfig(),
          perfLogs: perfLogsFromConfig(),
        },
      },
    },
  };

  client = new LanguageClient('fossilsense', 'FossilSense', serverOptions, clientOptions);
  client.setTrace(traceFromConfig());
  client.onNotification('fossilsense/indexStatus', (status: IndexStatus) => {
    handleIndexStatus(status);
    if (status.state === 'ready') {
      void applyProjectContextSelectionFromState(context).then(() =>
        updateProjectContextForActiveEditor(context),
      );
    }
  });
  client.onNotification('fossilsense/projectContextChanged', () => {
    void applyProjectContextSelectionFromState(context).then(() =>
      updateProjectContextForActiveEditor(context),
    );
  });

  try {
    await client.start();
    setStatus('ready');
    await applyProjectContextSelectionFromState(context);
    await updateProjectContextForActiveEditor(context);
    if (fossilsenseMode === 'auto' && conflictingExtensions.length > 0) {
      void showMutualExclusionWarning(conflictingExtensions);
    }
  } catch (error) {
    client = undefined;
    setStatus('scan failed');
    output.appendLine(`Failed to start FossilSense: ${String(error)}`);
    void vscode.window.showErrorMessage(`Failed to start FossilSense: ${String(error)}`);
  }
}

async function stopServer(): Promise<void> {
  const current = client;
  client = undefined;
  configWarning = undefined;
  currentIndexStartedWithWarning = false;
  projectContextPromptTracker.clear();
  projectContextUpdateEpoch += 1;
  callRelationsController?.clear();

  if (current) {
    await current.stop();
  }

  setStatus('stopped');
  setProjectContextStatus(undefined);
}

async function refreshIndex(): Promise<void> {
  if (!client) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }

  output.appendLine('Refreshing index (incremental)...');
  setStatus('refreshing...');
  await client.sendRequest(ExecuteCommandRequest.type, {
    command: REFRESH_INDEX_LSP_COMMAND,
    arguments: [],
  });
}

async function rebuildIndex(): Promise<void> {
  if (!client) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }

  output.appendLine('Full rebuild index (force)...');
  setStatus('full rebuild...');
  await client.sendRequest(ExecuteCommandRequest.type, {
    command: REBUILD_INDEX_LSP_COMMAND,
    arguments: [],
  });
}

async function clearCompletionHistory(): Promise<void> {
  if (!client) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }

  output.appendLine('Clearing local completion history...');
  await client.sendRequest(ExecuteCommandRequest.type, clearCompletionHistoryRequest());
}

async function applyProjectContextSelectionFromState(
  context: vscode.ExtensionContext,
): Promise<void> {
  if (!client) {
    return;
  }
  const status = await requestProjectContextStatus();
  if (!status) {
    setProjectContextStatus(undefined);
    return;
  }

  const mode = projectContextModeFromConfig();
  if (!status.available) {
    const initial: ProjectContextSelection =
      mode === 'off' ? { kind: 'unspecified' } : { kind: 'auto' };
    setProjectContextStatus((await sendProjectContextSelection(initial)) ?? status);
    return;
  }

  const stored = context.workspaceState.get(PROJECT_CONTEXT_WORKSPACE_STATE_KEY);
  const validStored = validStoredProjectContextSelection(stored, status.projects);
  if (stored !== undefined && validStored === undefined) {
    await context.workspaceState.update(PROJECT_CONTEXT_WORKSPACE_STATE_KEY, undefined);
  }
  const selection = effectiveSelectionForMode(mode, validStored);
  setProjectContextStatus((await sendProjectContextSelection(selection)) ?? status);
}

async function updateProjectContextForActiveEditor(
  context: vscode.ExtensionContext,
): Promise<void> {
  if (!client) {
    setProjectContextStatus(undefined);
    return;
  }
  const updateEpoch = ++projectContextUpdateEpoch;
  const editor = vscode.window.activeTextEditor;
  const uri = editor?.document.uri.toString();
  const status = await requestProjectContextStatus(uri);
  if (
    updateEpoch !== projectContextUpdateEpoch ||
    vscode.window.activeTextEditor?.document.uri.toString() !== uri
  ) {
    return;
  }
  setProjectContextStatus(status);
  if (!editor || !isLocalCppDocument(editor.document)) {
    return;
  }
  if (!shouldPromptForProjectContext(projectContextModeFromConfig(), status)) {
    return;
  }
  const localUri = editor.document.uri.toString();
  if (!projectContextPromptTracker.claim(localUri)) {
    return;
  }
  await showProjectContextSelector(context, true, localUri);
}

async function showProjectContextSelector(
  context: vscode.ExtensionContext,
  prompted: boolean,
  expectedUri?: string,
): Promise<void> {
  if (!client) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }
  if (projectContextModeFromConfig() === 'off') {
    setProjectContextStatus(await sendProjectContextSelection({ kind: 'unspecified' }));
    void vscode.window.showInformationMessage(
      'FossilSense project context is disabled by fossilsense.projectContext.mode=off.',
    );
    return;
  }

  if (
    prompted &&
    expectedUri !== undefined &&
    vscode.window.activeTextEditor?.document.uri.toString() !== expectedUri
  ) {
    return;
  }

  const status = await requestProjectContextStatus();
  if (
    prompted &&
    expectedUri !== undefined &&
    vscode.window.activeTextEditor?.document.uri.toString() !== expectedUri
  ) {
    return;
  }
  if (!status?.available) {
    setProjectContextStatus(status);
    void vscode.window.showInformationMessage(
      'FossilSense project context is not available yet; baseline completion remains active.',
    );
    return;
  }
  const rows = projectContextPickRows(status.projects).map((row) => ({
    label: row.label,
    description: row.description,
    row,
  }));
  const chosen = await vscode.window.showQuickPick(rows, {
    placeHolder: prompted
      ? 'FossilSense could not infer this file\'s project. Choose a project context.'
      : 'FossilSense project context for ordinary completion',
    matchOnDescription: true,
  });
  if (!chosen) {
    return;
  }
  await context.workspaceState.update(
    PROJECT_CONTEXT_WORKSPACE_STATE_KEY,
    chosen.row.selection,
  );
  // A user choice wins over any status request that started before the
  // QuickPick completed.
  projectContextUpdateEpoch += 1;
  setProjectContextStatus(
    (await sendProjectContextSelection(chosen.row.selection)) ?? status,
  );
}

async function requestProjectContextStatus(
  uri?: string,
): Promise<ProjectContextStatus | undefined> {
  const current = client;
  if (!current) {
    return undefined;
  }
  try {
    return (await current.sendRequest(ExecuteCommandRequest.type, {
      command: PROJECT_CONTEXTS_LSP_COMMAND,
      arguments: uriArgument(uri),
    })) as ProjectContextStatus | undefined;
  } catch (error) {
    output.appendLine(`Project context status request failed: ${String(error)}`);
    return undefined;
  }
}

async function sendProjectContextSelection(
  selection: ProjectContextSelection,
): Promise<ProjectContextStatus | undefined> {
  const current = client;
  if (!current) {
    return undefined;
  }
  const effective =
    projectContextModeFromConfig() === 'off' ? { kind: 'unspecified' as const } : selection;
  const [uri] = activeEditorUriArgument();
  try {
    return (await current.sendRequest(ExecuteCommandRequest.type, {
      command: SET_PROJECT_CONTEXT_LSP_COMMAND,
      arguments: [{ selection: effective, ...(uri ?? {}) }],
    })) as ProjectContextStatus | undefined;
  } catch (error) {
    output.appendLine(`Project context selection request failed: ${String(error)}`);
    return undefined;
  }
}

function activeEditorUriArgument(): Array<{ uri: string }> {
  const uri = vscode.window.activeTextEditor?.document.uri;
  return uriArgument(uri?.toString());
}

function uriArgument(uri: string | undefined): Array<{ uri: string }> {
  return uri ? [{ uri }] : [];
}

function isLocalCppDocument(document: vscode.TextDocument): boolean {
  return (
    document.uri.scheme === 'file' &&
    (document.languageId === 'c' || document.languageId === 'cpp')
  );
}

// Role-grouped find-references. The standard References panel (textDocument/
// references) returns plain Locations in role-grouped order but cannot show a
// per-item role; this command asks the server for the same hits *with* their
// best-effort syntactic role and presents them grouped and labeled in a
// QuickPick. Roles are syntactic guesses, not resolved bindings.
async function findReferencesGrouped(): Promise<void> {
  if (!client) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    void vscode.window.showInformationMessage('Open a C/C++ file and place the cursor on an identifier.');
    return;
  }
  const { document, selection } = editor;
  const position = selection.active;

  const items = (await client.sendRequest(ExecuteCommandRequest.type, {
    command: GROUPED_REFERENCES_LSP_COMMAND,
    arguments: [
      {
        uri: document.uri.toString(),
        line: position.line,
        character: position.character,
      },
    ],
  })) as GroupedReferenceItem[] | null;

  if (!items || items.length === 0) {
    void vscode.window.showInformationMessage('FossilSense: no references found.');
    return;
  }

  // Build a QuickPick with a separator per role group; items already arrive in
  // role-grouped order from the server, so a role change starts a new section.
  const picks = groupedReferencePickRows(
    items,
    showReferenceRangesFromConfig(),
    (uri) => vscode.workspace.asRelativePath(vscode.Uri.parse(uri)),
  ).map((row): vscode.QuickPickItem & { item?: GroupedReferenceItem } => {
    if (row.kind === 'separator') {
      return { label: row.label, kind: vscode.QuickPickItemKind.Separator };
    }
    return { label: row.label, description: row.description, item: row.item };
  });

  const chosen = await vscode.window.showQuickPick(picks, {
    placeHolder: `FossilSense references (${items.length}), grouped by role`,
    matchOnDescription: true,
  });
  if (!chosen?.item) {
    return;
  }
  const target = chosen.item.location;
  const uri = vscode.Uri.parse(target.uri);
  const range = new vscode.Range(
    target.range.start.line,
    target.range.start.character,
    target.range.end.line,
    target.range.end.character,
  );
  await vscode.window.showTextDocument(uri, { selection: range });
}

function resolveServerPath(context: vscode.ExtensionContext): string | undefined {
  const configured = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('serverPath', '')
    .trim();
  return resolveServerPathFromCandidates({
    platform: process.platform,
    configuredPath: configured,
    extensionPath: context.extensionPath,
    exists: fs.existsSync,
  });
}

function includePathsFromConfig(): string[] {
  return vscode.workspace
    .getConfiguration('fossilsense')
    .get<string[]>('includePaths', [])
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
}

function debugCandidateReasonsFromConfig(): boolean {
  return vscode.workspace
    .getConfiguration('fossilsense')
    .get<boolean>('debug.candidateReasons', false);
}

function showReferenceRangesFromConfig(): boolean {
  return vscode.workspace
    .getConfiguration('fossilsense')
    .get<boolean>('references.showRanges', false);
}

function fossilsenseModeFromConfig(): string {
  const setting = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('mode', 'auto');
  return normalizeOnOffAuto(setting);
}

// Limited include-reachability scoping. Unlike completion/coloring there is no
// conflict deference: scoping only narrows FossilSense's own output, so it is
// either auto (on) or off.
function includeScopingModeFromConfig(): string {
  const setting = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('includeScoping.mode', 'auto');
  return normalizeIncludeScopingMode(setting);
}

function traceFromConfig(): Trace {
  const value = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('trace.server', 'off');

  switch (value) {
    case 'messages':
      return Trace.Messages;
    case 'verbose':
      return Trace.Verbose;
    default:
      return Trace.Off;
  }
}

function perfLogsFromConfig(): boolean {
  return vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('trace.server', 'off') === 'verbose';
}

function completionModeFromConfig(): string {
  const setting = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('completion.mode', 'auto');

  return normalizeOnOffAuto(setting);
}

function completionHistoryModeFromConfig(): string {
  const setting = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('completionHistory.mode', 'auto');

  return normalizeOnOffAuto(setting);
}

function projectContextModeFromConfig(): ReturnType<typeof normalizeProjectContextMode> {
  const setting = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('projectContext.mode', 'auto');
  return normalizeProjectContextMode(setting);
}

function semanticColoringModeFromConfig(): string {
  const setting = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('semanticColoring.mode', 'auto');

  return normalizeOnOffAuto(setting);
}

function detectedCppLanguageServers(): string[] {
  return CONFLICT_EXTENSIONS.filter((extension) => {
    return vscode.extensions.getExtension(extension.id) !== undefined;
  }).map((extension) => extension.name);
}

async function showMutualExclusionWarning(conflictingExtensions: string[]): Promise<void> {
  if (mutualExclusionWarningShown) {
    return;
  }
  mutualExclusionWarningShown = true;

  const msg = mutualExclusionMessage(conflictingExtensions);
  output.appendLine(`Mutual-exclusion notice: ${msg}`);

  const stop = 'Stop FossilSense';
  const settings = 'Open Settings';
  const selected = await vscode.window.showWarningMessage(msg, stop, settings);
  if (selected === stop) {
    await stopServer();
  } else if (selected === settings) {
    await vscode.commands.executeCommand('workbench.action.openSettings', 'fossilsense.mode');
  }
}

function handleIndexStatus(status: IndexStatus): void {
  switch (status.state) {
    case 'indexing':
      if (status.message) {
        configWarning = status.message;
        currentIndexStartedWithWarning = true;
        output.appendLine(`Config warning: ${status.message}`);
      } else if (status.processedFiles === 0 && !currentIndexStartedWithWarning) {
        configWarning = undefined;
        capabilityWarning = undefined;
      } else if (status.processedFiles === 0) {
        currentIndexStartedWithWarning = false;
      }
      setStatus(indexingStatusText(status));
      break;
    case 'ready':
      capabilityWarning = degradedCapabilityWarning(status.degradedCapabilities);
      setStatus('ready');
      output.appendLine(
        `Index ready: ${status.workspace}; files=${status.totalFiles}, indexed=${status.indexedFiles}, skipped=${status.skippedFiles}, symbols=${status.symbols}, elapsed=${status.elapsedMs}ms (discover=${status.discoverMs}ms, check=${status.checkMs}ms, parse=${status.parseMs}ms, write=${status.writeMs}ms, include_edge=${status.includeEdgeMs}ms, name_table=${status.nameTableMs}ms, reach_graph=${status.reachGraphMs}ms)${capabilityWarning ? `; degraded=${capabilityWarning}` : ''}`,
      );
      break;
    case 'failed':
      capabilityWarning = undefined;
      setStatus('failed');
      output.appendLine(`Index failed: ${status.workspace}; ${status.message ?? 'unknown error'}`);
      break;
  }
}

function indexingStatusText(status: IndexStatus): string {
  const phase = status.phase ?? 'indexing';
  if (phase === 'discovering') {
    return 'discovering...';
  }
  if (phase === 'finalizing') {
    return 'finalizing...';
  }
  if (status.totalFiles === 0) {
    return `${phase}...`;
  }
  return `${phase} ${status.processedFiles}/${status.totalFiles}`;
}

function setStatus(state: string): void {
  const warningSuffix = configWarning || capabilityWarning ? ' [!]' : '';
  statusBar.text = `FossilSense: ${state}${warningSuffix}`;
  statusBar.tooltip = statusTooltip(configWarning, capabilityWarning);
  statusBar.backgroundColor = configWarning || capabilityWarning
    ? new vscode.ThemeColor('statusBarItem.warningBackground')
    : undefined;
}

function setProjectContextStatus(status: ProjectContextStatus | undefined): void {
  const mode = projectContextModeFromConfig();
  projectContextStatusBar.text = projectContextStatusText(mode, status);
  projectContextStatusBar.tooltip = projectContextTooltip(mode, status);
  projectContextStatusBar.command = SELECT_PROJECT_CONTEXT_COMMAND;
}
