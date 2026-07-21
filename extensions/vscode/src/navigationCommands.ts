import * as vscode from 'vscode';
import { ExecuteCommandRequest, LanguageClient } from 'vscode-languageclient/node';
import { showReferenceRangesFromConfig } from './extensionConfig';
import { GroupedReferenceItem, groupedReferencePickRows } from './referencesView';
import {
  PossibleTargetItem,
  PossibleTargetsResponse,
  possibleTargetPickRows,
  possibleTargetsCoverageSummary,
} from './possibleTargets';

const GROUPED_REFERENCES_LSP_COMMAND = 'fossilsense.lsp.groupedReferences';
const POSSIBLE_TARGETS_LSP_COMMAND = 'fossilsense.lsp.possibleTargets';

export async function findAllPossibleTargets(client: LanguageClient | undefined): Promise<void> {
  if (!client) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    void vscode.window.showInformationMessage(
      'Open a C/C++ file and place the cursor on an identifier.',
    );
    return;
  }
  const { document, selection } = editor;
  const position = selection.active;
  const response = (await client.sendRequest(ExecuteCommandRequest.type, {
    command: POSSIBLE_TARGETS_LSP_COMMAND,
    arguments: [{ uri: document.uri.toString(), line: position.line, character: position.character }],
  })) as PossibleTargetsResponse | null;

  if (!response || response.items.length === 0) {
    void vscode.window.showInformationMessage('FossilSense: no possible targets found.');
    return;
  }
  const picks = possibleTargetPickRows(
    response.items,
    (uri) => vscode.workspace.asRelativePath(vscode.Uri.parse(uri)),
  ).map((row): vscode.QuickPickItem & { item?: PossibleTargetItem } =>
    row.kind === 'separator'
      ? { label: row.label, kind: vscode.QuickPickItemKind.Separator }
      : { label: row.label, description: row.description, detail: row.detail, item: row.item },
  );
  const coverage = possibleTargetsCoverageSummary(response.coverage);
  const chosen = await vscode.window.showQuickPick(picks, {
    placeHolder: `FossilSense ${response.name}: ${response.items.length} possible target(s)${coverage ? ` · ${coverage}` : ''}`,
    matchOnDescription: true,
    matchOnDetail: true,
  });
  if (chosen?.item) await showLocation(chosen.item.location);
}

export async function findReferencesGrouped(client: LanguageClient | undefined): Promise<void> {
  if (!client) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    void vscode.window.showInformationMessage('Open a C/C++ file and place the cursor on an identifier.');
    return;
  }
  const position = editor.selection.active;
  const items = (await client.sendRequest(ExecuteCommandRequest.type, {
    command: GROUPED_REFERENCES_LSP_COMMAND,
    arguments: [{
      uri: editor.document.uri.toString(),
      line: position.line,
      character: position.character,
    }],
  })) as GroupedReferenceItem[] | null;
  if (!items?.length) {
    void vscode.window.showInformationMessage('FossilSense: no references found.');
    return;
  }
  const picks = groupedReferencePickRows(
    items,
    showReferenceRangesFromConfig(),
    (uri) => vscode.workspace.asRelativePath(vscode.Uri.parse(uri)),
  ).map((row): vscode.QuickPickItem & { item?: GroupedReferenceItem } =>
    row.kind === 'separator'
      ? { label: row.label, kind: vscode.QuickPickItemKind.Separator }
      : { label: row.label, description: row.description, item: row.item },
  );
  const chosen = await vscode.window.showQuickPick(picks, {
    placeHolder: `FossilSense references (${items.length}), grouped by role`,
    matchOnDescription: true,
  });
  if (chosen?.item) await showLocation(chosen.item.location);
}

async function showLocation(location: PossibleTargetItem['location']): Promise<void> {
  const uri = vscode.Uri.parse(location.uri);
  const range = new vscode.Range(
    location.range.start.line,
    location.range.start.character,
    location.range.end.line,
    location.range.end.character,
  );
  await vscode.window.showTextDocument(uri, { selection: range });
}
