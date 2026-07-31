import * as fs from 'fs';
import * as vscode from 'vscode';
import { Trace } from 'vscode-languageclient/node';
import {
  normalizeBoolean,
  normalizeCompletionPrefixRanking,
  normalizeExternalPathList,
  normalizeIncludeScopingMode,
  normalizeOnOffAuto,
  normalizeProjectContextMode,
} from './config';
import { resolveServerPathFromCandidates } from './serverPath';
import {
  activeLanguageProviderNames,
  CONFLICT_LANGUAGE_SERVER_EXTENSIONS,
} from './conflicts';

export function resolveServerPath(context: vscode.ExtensionContext): string | undefined {
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

export function includePathsFromConfig(): string[] {
  return normalizeExternalPathList(
    vscode.workspace.getConfiguration('fossilsense').get<unknown>('includePaths', []),
  );
}

export function goModulePathsFromConfig(): string[] {
  return normalizeExternalPathList(
    vscode.workspace.getConfiguration('fossilsense').get<unknown>('goModulePaths', []),
  );
}

export function debugCandidateReasonsFromConfig(): boolean {
  return vscode.workspace
    .getConfiguration('fossilsense')
    .get<boolean>('debug.candidateReasons', false);
}

export function showReferenceRangesFromConfig(): boolean {
  return vscode.workspace
    .getConfiguration('fossilsense')
    .get<boolean>('references.showRanges', false);
}

export function fossilsenseModeFromConfig(): string {
  return normalizeOnOffAuto(
    vscode.workspace.getConfiguration('fossilsense').get<string>('mode', 'auto'),
  );
}

export function resourceMonitorEnabledFromConfig(): boolean {
  const value = vscode.workspace
    .getConfiguration('fossilsense')
    .get<unknown>('resourceMonitor.enabled', true);
  return normalizeBoolean(value);
}

export function completionPrefixRankingFromConfig(): string {
  const setting = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('completion.prefixRanking', 'strict');
  return normalizeCompletionPrefixRanking(setting);
}

export function includeScopingModeFromConfig(): string {
  const setting = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('includeScoping.mode', 'auto');
  return normalizeIncludeScopingMode(setting);
}

export function semanticIndexMemoryBudgetMBFromConfig(): number {
  const value = vscode.workspace
    .getConfiguration('fossilsense')
    .get<number>('semanticIndex.memoryBudgetMB', 256);
  return Number.isFinite(value) ? Math.max(0, Math.min(16384, Math.trunc(value))) : 256;
}

export function traceFromConfig(): Trace {
  const value = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('trace.server', 'off');
  if (value === 'messages') return Trace.Messages;
  if (value === 'verbose') return Trace.Verbose;
  return Trace.Off;
}

export function perfLogsFromConfig(): boolean {
  return vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('trace.server', 'off') === 'verbose';
}

export function completionModeFromConfig(): string {
  return normalizeOnOffAuto(
    vscode.workspace.getConfiguration('fossilsense').get<string>('completion.mode', 'auto'),
  );
}

export function completionHistoryModeFromConfig(): string {
  return normalizeOnOffAuto(
    vscode.workspace
      .getConfiguration('fossilsense')
      .get<string>('completionHistory.mode', 'auto'),
  );
}

export function projectContextModeFromConfig(): ReturnType<typeof normalizeProjectContextMode> {
  const setting = vscode.workspace
    .getConfiguration('fossilsense')
    .get<string>('projectContext.mode', 'auto');
  return normalizeProjectContextMode(setting);
}

export function semanticColoringModeFromConfig(): string {
  return normalizeOnOffAuto(
    vscode.workspace
      .getConfiguration('fossilsense')
      .get<string>('semanticColoring.mode', 'auto'),
  );
}

export function detectedLanguageServers(): string[] {
  const workspaceLanguages = new Set(
    vscode.workspace.textDocuments
      .filter(
        (document) =>
          document.uri.scheme === 'file' &&
          vscode.workspace.getWorkspaceFolder(document.uri) !== undefined,
      )
      .map((document) => document.languageId),
  );
  const installedExtensions = CONFLICT_LANGUAGE_SERVER_EXTENSIONS.map((extension) => {
    const installed = vscode.extensions.getExtension(extension.id);
    return { id: extension.id, isActive: installed?.isActive === true };
  });
  return activeLanguageProviderNames(installedExtensions, workspaceLanguages);
}
