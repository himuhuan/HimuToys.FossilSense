import * as fs from 'fs';
import * as vscode from 'vscode';
import {
  CloseAction,
  ErrorAction,
  ExecuteCommandRequest,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  State,
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
import {
  CreatedService,
  ServiceInstanceScope,
  ServiceLifecycle,
  ServiceLifecycleState,
} from './extensionLifecycle';

const REFRESH_INDEX_COMMAND = 'fossilsense.refreshIndex';
const REFRESH_INDEX_LSP_COMMAND = 'fossilsense.lsp.refreshIndex';
const REBUILD_INDEX_COMMAND = 'fossilsense.rebuildIndex';
const REBUILD_INDEX_LSP_COMMAND = 'fossilsense.lsp.rebuildIndex';
const GROUPED_REFERENCES_COMMAND = 'fossilsense.findReferencesGrouped';
const POSSIBLE_TARGETS_COMMAND = 'fossilsense.findAllPossibleTargets';
let serviceLifecycle: ServiceLifecycle<LanguageClient> | undefined;
let statusBar: vscode.StatusBarItem;
let projectContextStatusBar: vscode.StatusBarItem;
let resourceStatusBar: vscode.StatusBarItem;
let callRelationsController: CallRelationsController;
let output: vscode.OutputChannel;
let configWarning: string | undefined;
let capabilityWarning: string | undefined;
let currentIndexStartedWithWarning = false;
let mutualExclusionWarningShown = false;
const projectContextPromptTracker = new ProjectContextPromptTracker();
let projectContextUpdateEpoch = 0;

interface IndexStatus {
  state: 'indexing' | 'deferred' | 'ready' | 'failed';
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
  callRelationsController = registerCallRelationViews(context, () => serviceLifecycle?.client);
  serviceLifecycle = createServiceLifecycle(context);

  context.subscriptions.push(
    output,
    statusBar,
    projectContextStatusBar,
    resourceStatusBar,
    vscode.commands.registerCommand('fossilsense.startServer', () => serviceLifecycle?.start()),
    vscode.commands.registerCommand('fossilsense.stopServer', () => serviceLifecycle?.stop()),
    vscode.commands.registerCommand(REFRESH_INDEX_COMMAND, () => refreshIndex()),
    vscode.commands.registerCommand(REBUILD_INDEX_COMMAND, () => rebuildIndex()),
    vscode.commands.registerCommand(GROUPED_REFERENCES_COMMAND, () =>
      findReferencesGrouped(serviceLifecycle?.client),
    ),
    vscode.commands.registerCommand(POSSIBLE_TARGETS_COMMAND, () =>
      findAllPossibleTargets(serviceLifecycle?.client),
    ),
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
        await serviceLifecycle?.restart();
        return;
      }
      const current = serviceLifecycle?.client;
      if (event.affectsConfiguration('fossilsense.projectContext.mode') && current) {
        await applyProjectContextSelectionFromState(context, current);
        await updateProjectContextForActiveEditor(context, current);
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
        current &&
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
        await serviceLifecycle?.restart();
      }
    }),
    vscode.workspace.onDidChangeWorkspaceFolders(async () => {
      if (!serviceLifecycle?.client) {
        return;
      }
      output.appendLine('Workspace folders changed; restarting to refresh file watchers.');
      if (vscode.workspace.workspaceFolders?.length) {
        await serviceLifecycle.restart();
      } else {
        await serviceLifecycle.stop();
      }
    }),
  );

  // Auto-start when a workspace is open; the manual command stays as a fallback.
  if (vscode.workspace.workspaceFolders?.length) {
    void serviceLifecycle.start();
  }
}

export async function deactivate(): Promise<void> {
  await serviceLifecycle?.deactivate();
}

function createServiceLifecycle(
  context: vscode.ExtensionContext,
): ServiceLifecycle<LanguageClient> {
  return new ServiceLifecycle<LanguageClient>({
    create: (scope) => createServiceInstance(context, scope),
    stateChanged: handleServiceLifecycleState,
    backgroundError: (error) => {
      output.appendLine(`FossilSense lifecycle cleanup failed: ${String(error)}`);
    },
    stopTimeoutMs: 5000,
    // vscode-languageclient waits another two seconds before terminating a
    // process retained after a shutdown timeout. Do not replace that process
    // until the dependency's cleanup window has elapsed.
    stopFailureRecoveryDelayMs: 2500,
  });
}

function handleServiceLifecycleState(state: ServiceLifecycleState, error?: unknown): void {
  switch (state) {
    case 'starting':
      setStatus('starting');
      break;
    case 'ready':
      setStatus('ready');
      break;
    case 'stopping':
      clearServicePresentation('stopping...');
      break;
    case 'stopped':
    case 'deactivated':
      clearServicePresentation('stopped');
      break;
    case 'startFailed':
      clearServicePresentation('scan failed');
      output.appendLine(`Failed to start FossilSense: ${String(error)}`);
      void vscode.window.showErrorMessage(`Failed to start FossilSense: ${String(error)}`);
      break;
    case 'failed':
      clearServicePresentation('failed');
      output.appendLine(`FossilSense server stopped unexpectedly: ${String(error)}`);
      void vscode.window.showErrorMessage(
        `FossilSense server stopped unexpectedly. Run Start Server to retry: ${String(error)}`,
      );
      break;
    case 'stopFailed':
      clearServicePresentation('stop failed');
      output.appendLine(`Failed to stop FossilSense; retry Stop or Start: ${String(error)}`);
      void vscode.window.showErrorMessage(
        `Failed to stop FossilSense; its client handle was retained for retry: ${String(error)}`,
      );
      break;
  }
}

function clearServicePresentation(status: string): void {
  configWarning = undefined;
  currentIndexStartedWithWarning = false;
  projectContextPromptTracker.clear();
  projectContextUpdateEpoch += 1;
  callRelationsController?.clear();
  setStatus(status);
  setProjectContextStatus(undefined);
  resourceStatusBar.hide();
  resourceStatusBar.text = '';
  resourceStatusBar.tooltip = '';
}

function createServiceInstance(
  context: vscode.ExtensionContext,
  scope: ServiceInstanceScope,
): CreatedService<LanguageClient> | undefined {

  const fossilsenseMode = fossilsenseModeFromConfig();
  if (fossilsenseMode === 'off') {
    setStatus('disabled');
    output.appendLine('FossilSense is disabled by fossilsense.mode=off.');
    void vscode.window.showInformationMessage(
      'FossilSense is disabled by fossilsense.mode=off. Change the setting to start it.',
    );
    return undefined;
  }

  const workspaceFolders = vscode.workspace.workspaceFolders;
  const firstWorkspaceFolder = workspaceFolders?.[0];
  if (!firstWorkspaceFolder) {
    void vscode.window.showWarningMessage('Open a workspace folder before starting FossilSense.');
    return undefined;
  }

  const serverPath = resolveServerPath(context);
  if (!serverPath) {
    setStatus('scan failed');
    void vscode.window.showErrorMessage(
      'FossilSense server binary was not found. Run `cargo build` or set `fossilsense.serverPath`.',
    );
    return undefined;
  }

  output.appendLine(`Starting FossilSense server: ${serverPath}`);
  output.appendLine(
    `Workspaces: ${workspaceFolders.map((folder) => folder.uri.fsPath).join('; ')}`,
  );

  const serverOptions: ServerOptions = {
    command: serverPath,
    args: ['lsp'],
    options: {
      cwd: firstWorkspaceFolder.uri.fsPath,
      env: {
        ...process.env,
        FOSSILSENSE_RESOURCE_PROFILE: vscode.workspace.getConfiguration('fossilsense').get<string>('resources.profile', 'balanced'),
      },
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
  for (const watcher of fileEvents) {
    scope.own(watcher);
  }

  const conflictingExtensions = detectedLanguageServers();

  const completionMode = completionModeFromConfig();
  const completionHistoryMode = completionHistoryModeFromConfig();
  const semanticColoringMode = semanticColoringModeFromConfig();
  let reachedReady = false;

  const clientOptions: LanguageClientOptions = {
    documentSelector: languageDocumentSelectors(),
    outputChannel: output,
    errorHandler: {
      error: () => ({ action: ErrorAction.Continue, handled: true }),
      closed: () => {
        if (reachedReady) {
          scope.fail(new Error('language server connection closed'));
        }
        return { action: CloseAction.DoNotRestart, handled: true };
      },
    },
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

  const currentClient = new LanguageClient(
    'fossilsense',
    'FossilSense',
    serverOptions,
    clientOptions,
  );
  for (const watcher of configWatchers) {
    scope.own(
      watcher.onDidCreate(scope.guard(() => scheduleWatchPlanRestart())),
    );
    scope.own(
      watcher.onDidChange(scope.guard(() => scheduleWatchPlanRestart())),
    );
    scope.own(
      watcher.onDidDelete(scope.guard(() => scheduleWatchPlanRestart())),
    );
  }
  void currentClient.setTrace(traceFromConfig()).catch((error) => {
    if (scope.isCurrent()) {
      output.appendLine(`Failed to update FossilSense protocol trace: ${String(error)}`);
    }
  });
  scope.own(
    currentClient.onNotification(
      'fossilsense/indexStatus',
      scope.guard((status: IndexStatus) => {
        handleIndexStatus(status);
        if (status.state === 'ready') {
          callRelationsController.clear();
          runInstanceTask(scope, 'project context refresh', () =>
            refreshProjectContextForInstance(context, currentClient, scope),
          );
        }
      }),
    ),
  );
  scope.own(
    currentClient.onNotification(
      'fossilsense/projectContextChanged',
      scope.guard(() => {
        runInstanceTask(scope, 'project context notification', () =>
          refreshProjectContextForInstance(context, currentClient, scope),
        );
      }),
    ),
  );
  scope.own(
    currentClient.onNotification(
      'fossilsense/resourceUsage',
      scope.guard((usage: ResourceUsage) => {
        if (resourceMonitorEnabledFromConfig()) {
          setResourceStatus(usage);
        }
      }),
    ),
  );

  return {
    client: currentClient,
    isReady: () => currentClient.state === State.Running,
    onReady: async (readyScope) => {
      reachedReady = true;
      await refreshProjectContextForInstance(context, currentClient, readyScope);
      if (
        readyScope.isCurrent() &&
        fossilsenseMode === 'auto' &&
        conflictingExtensions.length > 0
      ) {
        void showMutualExclusionWarning(conflictingExtensions, readyScope);
      }
    },
  };
}

function runInstanceTask(
  scope: ServiceInstanceScope,
  description: string,
  task: () => Promise<void>,
): void {
  void task().catch((error) => {
    if (scope.isCurrent()) {
      output.appendLine(`FossilSense ${description} failed: ${String(error)}`);
    }
  });
}

async function refreshProjectContextForInstance(
  context: vscode.ExtensionContext,
  currentClient: LanguageClient,
  scope: ServiceInstanceScope,
): Promise<void> {
  await applyProjectContextSelectionFromState(context, currentClient);
  if (!scope.isCurrent()) {
    return;
  }
  await updateProjectContextForActiveEditor(context, currentClient);
}

function scheduleWatchPlanRestart(): void {
  output.appendLine('fossilsense.json changed; scheduling source-extension watcher refresh.');
  serviceLifecycle?.scheduleRestart(150);
}

async function refreshIndex(): Promise<void> {
  const current = serviceLifecycle?.client;
  if (!current) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }

  output.appendLine('Refreshing index (incremental)...');
  setStatus('refreshing...');
  await current.sendRequest(ExecuteCommandRequest.type, {
    command: REFRESH_INDEX_LSP_COMMAND,
    arguments: [],
  });
}

async function rebuildIndex(): Promise<void> {
  const current = serviceLifecycle?.client;
  if (!current) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }

  output.appendLine('Full rebuild index (force)...');
  setStatus('full rebuild...');
  await current.sendRequest(ExecuteCommandRequest.type, {
    command: REBUILD_INDEX_LSP_COMMAND,
    arguments: [],
  });
}

async function clearCompletionHistory(): Promise<void> {
  const current = serviceLifecycle?.client;
  if (!current) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }

  output.appendLine('Clearing local completion history...');
  await current.sendRequest(ExecuteCommandRequest.type, clearCompletionHistoryRequest());
}

async function applyProjectContextSelectionFromState(
  context: vscode.ExtensionContext,
  current: LanguageClient | undefined = serviceLifecycle?.client,
): Promise<void> {
  if (!current) {
    return;
  }
  const status = await requestProjectContextStatus(current);
  if (!isCurrentClient(current)) {
    return;
  }
  if (!status) {
    setProjectContextStatus(undefined);
    return;
  }

  const mode = projectContextModeFromConfig();
  if (!status.available) {
    const initial: ProjectContextSelection =
      mode === 'off' ? { kind: 'unspecified' } : { kind: 'auto' };
    const selected = await sendProjectContextSelection(current, initial);
    if (isCurrentClient(current)) {
      setProjectContextStatus(selected ?? status);
    }
    return;
  }

  const stored = context.workspaceState.get(PROJECT_CONTEXT_WORKSPACE_STATE_KEY);
  const validStored = validStoredProjectContextSelection(stored, status.projects);
  if (stored !== undefined && validStored === undefined) {
    await context.workspaceState.update(PROJECT_CONTEXT_WORKSPACE_STATE_KEY, undefined);
    if (!isCurrentClient(current)) {
      return;
    }
  }
  const selection = effectiveSelectionForMode(mode, validStored);
  const selected = await sendProjectContextSelection(current, selection);
  if (isCurrentClient(current)) {
    setProjectContextStatus(selected ?? status);
  }
}

async function updateProjectContextForActiveEditor(
  context: vscode.ExtensionContext,
  current: LanguageClient | undefined = serviceLifecycle?.client,
): Promise<void> {
  if (!current) {
    setProjectContextStatus(undefined);
    return;
  }
  const updateEpoch = ++projectContextUpdateEpoch;
  const editor = vscode.window.activeTextEditor;
  const uri = editor?.document.uri.toString();
  const status = await requestProjectContextStatus(current, uri);
  if (
    !isCurrentClient(current) ||
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
  await showProjectContextSelector(context, true, localUri, current);
}

async function showProjectContextSelector(
  context: vscode.ExtensionContext,
  prompted: boolean,
  expectedUri?: string,
  current: LanguageClient | undefined = serviceLifecycle?.client,
): Promise<void> {
  if (!current) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }
  if (projectContextModeFromConfig() === 'off') {
    const selected = await sendProjectContextSelection(current, { kind: 'unspecified' });
    if (!isCurrentClient(current)) {
      return;
    }
    setProjectContextStatus(selected);
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

  const status = await requestProjectContextStatus(current);
  if (
    !isCurrentClient(current) ||
    (prompted &&
      expectedUri !== undefined &&
      vscode.window.activeTextEditor?.document.uri.toString() !== expectedUri)
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
  if (!isCurrentClient(current)) {
    return;
  }
  await context.workspaceState.update(
    PROJECT_CONTEXT_WORKSPACE_STATE_KEY,
    chosen.row.selection,
  );
  if (!isCurrentClient(current)) {
    return;
  }
  // A user choice wins over any status request that started before the
  // QuickPick completed.
  projectContextUpdateEpoch += 1;
  const selected = await sendProjectContextSelection(current, chosen.row.selection);
  if (isCurrentClient(current)) {
    setProjectContextStatus(selected ?? status);
  }
}

async function requestProjectContextStatus(
  current: LanguageClient,
  uri?: string,
): Promise<ProjectContextStatus | undefined> {
  try {
    return (await current.sendRequest(ExecuteCommandRequest.type, {
      command: PROJECT_CONTEXTS_LSP_COMMAND,
      arguments: uriArgument(uri),
    })) as ProjectContextStatus | undefined;
  } catch (error) {
    if (isCurrentClient(current)) {
      output.appendLine(`Project context status request failed: ${String(error)}`);
    }
    return undefined;
  }
}

async function sendProjectContextSelection(
  current: LanguageClient,
  selection: ProjectContextSelection,
): Promise<ProjectContextStatus | undefined> {
  const effective =
    projectContextModeFromConfig() === 'off' ? { kind: 'unspecified' as const } : selection;
  const [uri] = activeEditorUriArgument();
  try {
    return (await current.sendRequest(ExecuteCommandRequest.type, {
      command: SET_PROJECT_CONTEXT_LSP_COMMAND,
      arguments: [{ selection: effective, ...(uri ?? {}) }],
    })) as ProjectContextStatus | undefined;
  } catch (error) {
    if (isCurrentClient(current)) {
      output.appendLine(`Project context selection request failed: ${String(error)}`);
    }
    return undefined;
  }
}

function isCurrentClient(current: LanguageClient): boolean {
  return serviceLifecycle?.isCurrentClient(current) ?? false;
}

function activeEditorUriArgument(): Array<{ uri: string }> {
  const uri = vscode.window.activeTextEditor?.document.uri;
  return uriArgument(uri?.toString());
}

function uriArgument(uri: string | undefined): Array<{ uri: string }> {
  return uri ? [{ uri }] : [];
}

async function showMutualExclusionWarning(
  conflictingExtensions: string[],
  scope: ServiceInstanceScope,
): Promise<void> {
  if (mutualExclusionWarningShown) {
    return;
  }
  mutualExclusionWarningShown = true;

  const msg = mutualExclusionMessage(conflictingExtensions);
  output.appendLine(`Mutual-exclusion notice: ${msg}`);

  const stop = 'Stop FossilSense';
  const settings = 'Open Settings';
  const selected = await vscode.window.showWarningMessage(msg, stop, settings);
  if (!scope.isCurrent()) {
    return;
  }
  if (selected === stop) {
    await serviceLifecycle?.stop();
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
    case 'deferred':
      setStatus('waiting for resources');
      output.appendLine(
        `Index deferred: ${status.workspace}; ${status.message ?? 'waiting for build resources'}`,
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

function setResourceStatus(usage: ResourceUsage): void {
  resourceStatusBar.text = resourceUsageStatusText(usage.memoryBytes, usage.indexDiskBytes);
  const tooltip = new vscode.MarkdownString(resourceUsageTooltip(usage));
  tooltip.supportHtml = false;
  resourceStatusBar.tooltip = tooltip;
  resourceStatusBar.show();
}
