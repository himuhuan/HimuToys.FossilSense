import * as fs from 'fs';
import * as vscode from 'vscode';
import { Trace } from 'vscode-languageclient/node';
import {
  normalizeBoolean,
  normalizeCompletionPrefixRanking,
  normalizeIncludeScopingMode,
  normalizeOnOffAuto,
  normalizeProjectContextMode,
} from './config';
import { resolveServerPathFromCandidates } from './serverPath';

const CONFLICT_EXTENSIONS = [
  { id: 'llvm-vs-code-extensions.vscode-clangd', name: 'clangd' },
  { id: 'ms-vscode.cpptools', name: 'Microsoft C/C++' },
  { id: 'ccls-project.ccls', name: 'ccls' },
];

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
  return vscode.workspace
    .getConfiguration('fossilsense')
    .get<string[]>('includePaths', [])
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
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

export function detectedCppLanguageServers(): string[] {
  return CONFLICT_EXTENSIONS.filter((extension) => {
    return vscode.extensions.getExtension(extension.id) !== undefined;
  }).map((extension) => extension.name);
}
