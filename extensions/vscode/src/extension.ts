import * as fs from 'fs';
import * as vscode from 'vscode';
import {
  ExecuteCommandRequest,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from 'vscode-languageclient/node';
import {
  completionHistoryModeFromConfig,
  completionModeFromConfig,
  completionPrefixRankingFromConfig,
  debugCandidateReasonsFromConfig,
  detectedLanguageServers,
  fossilsenseModeFromConfig,
  goModulePathsFromConfig,
  includePathsFromConfig,
  includeScopingModeFromConfig,
  perfLogsFromConfig,
  protobufCEnabledOverrideFromConfig,
  protobufCProtoPathsFromConfig,
  projectContextModeFromConfig,
  resolveServerPath,
  resourceMonitorEnabledFromConfig,
  semanticColoringModeFromConfig,
  semanticIndexMemoryBudgetMBFromConfig,
  traceFromConfig,
} from './extensionConfig';
import {
  CLEAR_COMPLETION_HISTORY_COMMAND,
  clearCompletionHistoryRequest,
  completionHistoryInitializationOptions,
} from './completionHistory';
import { extensionsFromConfigText, sourceWatchGlob } from './watchPlan';
import {
  PROJECT_CONTEXT_MARKER_PATTERNS,
  isSupportedLocalDocument,
  languageDocumentSelectors,
} from './languageSupport';
import {
  DegradedCapabilities,
  degradedCapabilityWarning,
  statusTooltip,
} from './status';
import { mutualExclusionMessage } from './conflicts';
import { findAllPossibleTargets, findReferencesGrouped } from './navigationCommands';
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
import {
  ResourceUsage,
  resourceUsageStatusText,
  resourceUsageTooltip,
} from './resourceUsage';

const REFRESH_INDEX_COMMAND = 'fossilsense.refreshIndex';
const REFRESH_INDEX_LSP_COMMAND = 'fossilsense.lsp.refreshIndex';
const REBUILD_INDEX_COMMAND = 'fossilsense.rebuildIndex';
const REBUILD_INDEX_LSP_COMMAND = 'fossilsense.lsp.rebuildIndex';
const GROUPED_REFERENCES_COMMAND = 'fossilsense.findReferencesGrouped';
const POSSIBLE_TARGETS_COMMAND = 'fossilsense.findAllPossibleTargets';
let client: LanguageClient | undefined;
let statusBar: vscode.StatusBarItem;
let projectContextStatusBar: vscode.StatusBarItem;
let resourceStatusBar: vscode.StatusBarItem;
let callRelationsController: CallRelationsController;
let output: vscode.OutputChannel;
let configWarning: string | undefined;
let capabilityWarning: string | undefined;
let currentIndexStartedWithWarning = false;
let mutualExclusionWarningShown = false;
let watchPlanRestarting = false;
let watchPlanListeners: vscode.Disposable[] = [];
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
  /** Compatibility wire name; the value is the canonical declaration count. */
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
  resourceStatusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 98);
  resourceStatusBar.tooltip = '';
  resourceStatusBar.hide();
  callRelationsController = registerCallRelationViews(context, () => client);

  context.subscriptions.push(
    output,
    statusBar,
    projectContextStatusBar,
    resourceStatusBar,
    vscode.commands.registerCommand('fossilsense.startServer', () => startServer(context)),
    vscode.commands.registerCommand('fossilsense.stopServer', () => stopServer()),
    vscode.commands.registerCommand(REFRESH_INDEX_COMMAND, () => refreshIndex()),
    vscode.commands.registerCommand(REBUILD_INDEX_COMMAND, () => rebuildIndex()),
    vscode.commands.registerCommand(GROUPED_REFERENCES_COMMAND, () => findReferencesGrouped(client)),
    vscode.commands.registerCommand(POSSIBLE_TARGETS_COMMAND, () => findAllPossibleTargets(client)),
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
      if (event.affectsConfiguration('fossilsense.resourceMonitor.enabled')) {
        if (!resourceMonitorEnabledFromConfig()) {
          resourceStatusBar.hide();
          resourceStatusBar.text = '';
          resourceStatusBar.tooltip = '';
        }
        // When turning on, do nothing here: the next `fossilsense/resourceUsage`
        // notification (if the server is running) will show the status bar.
        return;
      }
      if (
        client &&
        (event.affectsConfiguration('fossilsense.includePaths') ||
          event.affectsConfiguration('fossilsense.goModulePaths') ||
          event.affectsConfiguration('fossilsense.protobufC.enabled') ||
          event.affectsConfiguration('fossilsense.protobufC.protoPaths') ||
          event.affectsConfiguration('fossilsense.completion.mode') ||
          event.affectsConfiguration('fossilsense.completion.prefixRanking') ||
          event.affectsConfiguration('fossilsense.completionHistory.mode') ||
          event.affectsConfiguration('fossilsense.semanticColoring.mode') ||
          event.affectsConfiguration('fossilsense.includeScoping.mode') ||
          event.affectsConfiguration('fossilsense.semanticIndex.memoryBudgetMB') ||
          event.affectsConfiguration('fossilsense.debug.candidateReasons') ||
          event.affectsConfiguration('fossilsense.trace.server'))
      ) {
        output.appendLine('FossilSense configuration changed; restarting server.');
        await stopServer();
        await startServer(context);
      }
    }),
    vscode.workspace.onDidChangeWorkspaceFolders(async () => {
      if (!client) {
        return;
      }
      output.appendLine('Workspace folders changed; restarting to refresh file watchers.');
      await stopServer();
      if (vscode.workspace.workspaceFolders?.length) {
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

  const configWatchers = workspaceFolders.map((folder) =>
    vscode.workspace.createFileSystemWatcher(
      new vscode.RelativePattern(folder, 'fossilsense.json'),
    ),
  );
  const fileEvents = [
    ...workspaceFolders.flatMap((folder) => {
      const configPath = vscode.Uri.joinPath(folder.uri, 'fossilsense.json').fsPath;
      const configText = fs.existsSync(configPath) ? fs.readFileSync(configPath, 'utf8') : undefined;
      const sourceGlob = sourceWatchGlob(extensionsFromConfigText(configText));
      return sourceGlob
        ? [vscode.workspace.createFileSystemWatcher(new vscode.RelativePattern(folder, sourceGlob))]
        : [];
    }),
    ...configWatchers,
    ...workspaceFolders.flatMap((folder) =>
      PROJECT_CONTEXT_MARKER_PATTERNS.map((pattern) =>
        vscode.workspace.createFileSystemWatcher(new vscode.RelativePattern(folder, pattern)),
      ),
    ),
  ];

  const conflictingExtensions = detectedLanguageServers();

  const completionMode = completionModeFromConfig();
  const completionHistoryMode = completionHistoryModeFromConfig();
  const semanticColoringMode = semanticColoringModeFromConfig();

  const clientOptions: LanguageClientOptions = {
    documentSelector: languageDocumentSelectors(),
    outputChannel: output,
    synchronize: {
      fileEvents,
    },
    initializationOptions: {
      fossilsense: {
        completion: {
          mode: completionMode,
          prefixRanking: completionPrefixRankingFromConfig(),
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
        semanticIndex: {
          memoryBudgetMB: semanticIndexMemoryBudgetMBFromConfig(),
        },
        includePaths: includePathsFromConfig(),
        goModulePaths: goModulePathsFromConfig(),
        protobufC: {
          enabled: protobufCEnabledOverrideFromConfig(),
          protoPaths: protobufCProtoPathsFromConfig(),
        },
        debug: {
          candidateReasons: debugCandidateReasonsFromConfig(),
          perfLogs: perfLogsFromConfig(),
        },
      },
    },
  };

  client = new LanguageClient('fossilsense', 'FossilSense', serverOptions, clientOptions);
  for (const watcher of configWatchers) {
    watchPlanListeners.push(
      watcher.onDidCreate(() => scheduleWatchPlanRestart(context)),
      watcher.onDidChange(() => scheduleWatchPlanRestart(context)),
      watcher.onDidDelete(() => scheduleWatchPlanRestart(context)),
    );
  }
  client.setTrace(traceFromConfig());
  client.onNotification('fossilsense/indexStatus', (status: IndexStatus) => {
    handleIndexStatus(status);
    if (status.state === 'ready') {
      callRelationsController.clear();
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
  client.onNotification('fossilsense/resourceUsage', (usage: ResourceUsage) => {
    if (!resourceMonitorEnabledFromConfig()) {
      return;
    }
    setResourceStatus(usage.memoryBytes, usage.indexDiskBytes);
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
  for (const listener of watchPlanListeners.splice(0)) {
    listener.dispose();
  }

  if (current) {
    await current.stop();
  }

  setStatus('stopped');
  setProjectContextStatus(undefined);
  resourceStatusBar.hide();
  resourceStatusBar.text = '';
  resourceStatusBar.tooltip = '';
}

function scheduleWatchPlanRestart(context: vscode.ExtensionContext): void {
  if (watchPlanRestarting) {
    return;
  }
  watchPlanRestarting = true;
  setTimeout(() => {
    void (async () => {
      try {
        output.appendLine('fossilsense.json changed; refreshing source-extension watchers.');
        await stopServer();
        if (vscode.workspace.workspaceFolders?.length) {
          await startServer(context);
        }
      } finally {
        watchPlanRestarting = false;
      }
    })();
  }, 150);
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
  if (
    !editor ||
    !isSupportedLocalDocument(editor.document.uri.scheme, editor.document.languageId)
  ) {
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
        `Index ready: ${status.workspace}; files=${status.totalFiles}, indexed=${status.indexedFiles}, skipped=${status.skippedFiles}, declarations=${status.symbols}, elapsed=${status.elapsedMs}ms (discover=${status.discoverMs}ms, check=${status.checkMs}ms, parse=${status.parseMs}ms, write=${status.writeMs}ms, include_edge=${status.includeEdgeMs}ms, name_table=${status.nameTableMs}ms, reach_graph=${status.reachGraphMs}ms)${capabilityWarning ? `; degraded=${capabilityWarning}` : ''}`,
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

function setResourceStatus(memoryBytes: number, diskBytes: number): void {
  resourceStatusBar.text = resourceUsageStatusText(memoryBytes, diskBytes);
  resourceStatusBar.tooltip = resourceUsageTooltip(memoryBytes, diskBytes);
  resourceStatusBar.show();
}
