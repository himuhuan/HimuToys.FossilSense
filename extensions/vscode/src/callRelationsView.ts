import * as path from 'path';
import * as vscode from 'vscode';
import { ExecuteCommandRequest, LanguageClient } from 'vscode-languageclient/node';
import {
  CallRelation,
  CallSiteFact,
  RelationDirection,
  RichRelationResponse,
  RichRelationWireResponse,
  coverageSummary,
  evidenceSummary,
  normalizeRichRelationResponse,
  relationEntity,
} from './callRelationsModel';

export const SHOW_CALL_RELATIONS_COMMAND = 'fossilsense.showCallRelations';
export const REFRESH_CALL_RELATIONS_COMMAND = 'fossilsense.refreshCallRelations';
export const SHOW_INCOMING_CALLS_COMMAND = 'fossilsense.showIncomingCalls';
export const SHOW_OUTGOING_CALLS_COMMAND = 'fossilsense.showOutgoingCalls';
export const SELECT_CALL_RELATION_COMMAND = 'fossilsense.selectCallRelation';
export const OPEN_CALL_SITE_COMMAND = 'fossilsense.openCallSite';
export const LOAD_MORE_CALL_RELATIONS_COMMAND = 'fossilsense.loadMoreCallRelations';
const CALL_RELATIONS_LSP_COMMAND = 'fossilsense.lsp.callRelations';

interface PreparedCallItem {
  name: string;
  detail?: string;
  uri: string;
  selectionRange: { start: { line: number; character: number } };
  data?: { entityKey?: string };
}

class RelationItem extends vscode.TreeItem {
  constructor(
    readonly relation: CallRelation,
    direction: RelationDirection,
  ) {
    const entity = relationEntity(relation, direction);
    super(entity?.qualifiedName ?? relation.callSites[0]?.calleeName ?? 'Unresolved call');
    this.description = `${relation.confidence} · ${relation.callSites.length} site${relation.callSites.length === 1 ? '' : 's'}`;
    this.tooltip = new vscode.MarkdownString(
      `**${entity?.signature.normalized || String(this.label)}**\n\nConfidence: **${relation.confidence}**\n\n${evidenceSummary(relation)}`,
    );
    this.iconPath = new vscode.ThemeIcon(entity ? 'references' : 'question');
    this.contextValue = entity ? 'fossilsense.callRelation' : 'fossilsense.unresolvedCallRelation';
    this.command = {
      command: SELECT_CALL_RELATION_COMMAND,
      title: 'Show Call Sites',
      arguments: [relation],
    };
  }
}

type RelationNode =
  | { kind: 'status'; response: RichRelationResponse; direction: RelationDirection }
  | { kind: 'relation'; relation: CallRelation };

class RelationStatusItem extends vscode.TreeItem {
  constructor(response: RichRelationResponse, direction: RelationDirection) {
    super(`${direction === 'incoming' ? 'Incoming' : 'Outgoing'} · ${response.relations.length} relations`);
    this.description = response.complete ? 'complete' : response.budgetState;
    this.tooltip = coverageSummary(response.coverage);
    this.iconPath = new vscode.ThemeIcon(direction === 'incoming' ? 'arrow-left' : 'arrow-right');
    if (response.nextCursor) {
      this.command = {
        command: LOAD_MORE_CALL_RELATIONS_COMMAND,
        title: 'Load More Call Relations',
      };
    }
  }
}

type SiteNode =
  | { kind: 'coverage'; response: RichRelationResponse }
  | { kind: 'evidence'; relation: CallRelation }
  | { kind: 'site'; relation: CallRelation; site: CallSiteFact };

class SiteItem extends vscode.TreeItem {
  constructor(node: SiteNode, workspaceFolder: vscode.WorkspaceFolder | undefined) {
    if (node.kind === 'coverage') {
      super('Coverage');
      this.description = coverageSummary(node.response.coverage);
      this.tooltip = `${this.description} · generation ${node.response.revision.semanticGeneration}`;
      this.iconPath = new vscode.ThemeIcon('database');
      return;
    }
    if (node.kind === 'evidence') {
      super(`Evidence · ${node.relation.confidence}`);
      this.description = evidenceSummary(node.relation);
      this.tooltip = this.description;
      this.iconPath = new vscode.ThemeIcon('info');
      this.contextValue = 'fossilsense.callEvidence';
      return;
    }
    const line = node.site.calleeRange.start.line + 1;
    super(`${displayPath(node.site.path, workspaceFolder)}:${line}`);
    this.description = `${node.site.form}${node.site.argumentCount === undefined ? '' : ` · ${node.site.argumentCount} args`}`;
    this.tooltip = `${evidenceSummary(node.relation)}\n${node.site.path}:${line}`;
    this.iconPath = new vscode.ThemeIcon('location');
    this.contextValue = 'fossilsense.callSite';
    this.command = {
      command: OPEN_CALL_SITE_COMMAND,
      title: 'Open Call Site',
      arguments: [node.site, workspaceFolder],
    };
  }
}

export class CallRelationsController {
  private readonly relationEmitter = new vscode.EventEmitter<void>();
  private readonly siteEmitter = new vscode.EventEmitter<void>();
  private response: RichRelationResponse | undefined;
  private selected: CallRelation | undefined;
  private direction: RelationDirection = 'incoming';
  private lastLocation: { uri: vscode.Uri; line: number; character: number } | undefined;
  private workspaceFolder: vscode.WorkspaceFolder | undefined;
  private requestSerial = 0;
  private rootEntityKey: string | undefined;

  readonly relationProvider: vscode.TreeDataProvider<RelationNode> = {
    onDidChangeTreeData: this.relationEmitter.event,
    getTreeItem: (node) =>
      node.kind === 'status'
        ? new RelationStatusItem(node.response, node.direction)
        : new RelationItem(node.relation, this.direction),
    getChildren: () =>
      this.response
        ? [
            { kind: 'status', response: this.response, direction: this.direction },
            ...this.response.relations.map(
              (relation): RelationNode => ({ kind: 'relation', relation }),
            ),
          ]
        : [],
  };

  readonly siteProvider: vscode.TreeDataProvider<SiteNode> = {
    onDidChangeTreeData: this.siteEmitter.event,
    getTreeItem: (node) => new SiteItem(node, this.workspaceFolder),
    getChildren: () =>
      this.response
        ? [
            { kind: 'coverage', response: this.response } as SiteNode,
            ...(this.selected
              ? [
                  { kind: 'evidence', relation: this.selected } as SiteNode,
                  ...this.selected.callSites.map(
                    (site): SiteNode => ({ kind: 'site', relation: this.selected!, site }),
                  ),
                ]
              : []),
          ]
        : [],
  };

  constructor(private readonly getClient: () => LanguageClient | undefined) {}

  clear(): void {
    this.response = undefined;
    this.selected = undefined;
    this.lastLocation = undefined;
    this.rootEntityKey = undefined;
    this.requestSerial += 1;
    this.relationEmitter.fire();
    this.siteEmitter.fire();
  }

  async select(relation: CallRelation): Promise<void> {
    this.selected = relation;
    this.siteEmitter.fire();
    const entity = relationEntity(relation, this.direction);
    if (entity) {
      await openSourceRange(
        entity.primaryAnchor.path,
        entity.primaryAnchor.nameRange,
        this.workspaceFolder,
      );
    }
  }

  async show(direction: RelationDirection, refreshPosition = true): Promise<void> {
    const active = vscode.window.activeTextEditor;
    if (refreshPosition) {
      if (!active || !['c', 'cpp'].includes(active.document.languageId)) {
        void vscode.window.showInformationMessage('Place the cursor on a C/C++ function first.');
        return;
      }
      this.lastLocation = {
        uri: active.document.uri,
        line: active.selection.active.line,
        character: active.selection.active.character,
      };
      this.workspaceFolder = vscode.workspace.getWorkspaceFolder(active.document.uri);
      this.rootEntityKey = undefined;
    }
    const client = this.getClient();
    if (!client || !this.lastLocation) {
      void vscode.window.showInformationMessage('FossilSense is not ready.');
      return;
    }
    this.direction = direction;
    const requestSerial = ++this.requestSerial;
    let response: RichRelationResponse | null;
    try {
      if (!this.rootEntityKey) {
        const prepared = await client.sendRequest<PreparedCallItem[] | null>(
          'textDocument/prepareCallHierarchy',
          {
            textDocument: { uri: this.lastLocation.uri.toString() },
            position: {
              line: this.lastLocation.line,
              character: this.lastLocation.character,
            },
          },
        );
        if (requestSerial !== this.requestSerial) {
          return;
        }
        if (!prepared?.length) {
          this.response = undefined;
          this.selected = undefined;
          this.relationEmitter.fire();
          this.siteEmitter.fire();
          void vscode.window.showInformationMessage('No callable function at the current cursor.');
          return;
        }
        let selected = prepared[0];
        if (prepared.length > 1) {
          const pick = await vscode.window.showQuickPick(
            prepared.map((item) => ({
              label: item.name,
              description: item.detail,
              detail: `${vscode.workspace.asRelativePath(vscode.Uri.parse(item.uri), false)}:${item.selectionRange.start.line + 1}`,
              item,
            })),
            {
              placeHolder:
                'Select the callable root; FossilSense will not guess among candidates.',
            },
          );
          if (!pick || requestSerial !== this.requestSerial) {
            return;
          }
          selected = pick.item;
        }
        this.rootEntityKey = selected.data?.entityKey;
        if (!this.rootEntityKey) {
          throw new Error('server returned a call hierarchy item without a stable entity key');
        }
      }
      const wire = (await client.sendRequest(ExecuteCommandRequest.type, {
        command: CALL_RELATIONS_LSP_COMMAND,
        arguments: [
          {
            ...this.lastLocation,
            uri: this.lastLocation.uri.toString(),
            direction,
            entityKey: this.rootEntityKey,
          },
        ],
      })) as RichRelationWireResponse | null;
      response = wire ? normalizeRichRelationResponse(wire) : null;
    } catch (error) {
      if (requestSerial === this.requestSerial) {
        void vscode.window.showWarningMessage(`FossilSense call relations failed: ${String(error)}`);
      }
      return;
    }
    if (requestSerial !== this.requestSerial) {
      return;
    }
    this.response = response ?? undefined;
    this.selected = this.response?.relations[0];
    this.relationEmitter.fire();
    this.siteEmitter.fire();
    if (!response) {
      void vscode.window.showInformationMessage('No callable function at the current cursor.');
    }
  }

  async refresh(): Promise<void> {
    this.rootEntityKey = undefined;
    await this.show(this.direction, false);
  }

  async loadMore(): Promise<void> {
    const client = this.getClient();
    const cursor = this.response?.nextCursor;
    if (!client || !cursor || !this.lastLocation || !this.rootEntityKey) {
      return;
    }
    const requestSerial = ++this.requestSerial;
    try {
      const wire = (await client.sendRequest(ExecuteCommandRequest.type, {
        command: CALL_RELATIONS_LSP_COMMAND,
        arguments: [
          {
            ...this.lastLocation,
            uri: this.lastLocation.uri.toString(),
            direction: this.direction,
            entityKey: this.rootEntityKey,
            cursor,
          },
        ],
      })) as RichRelationWireResponse | null;
      if (!wire || requestSerial !== this.requestSerial) {
        return;
      }
      const next = normalizeRichRelationResponse(wire);
      this.response = {
        ...next,
        relations: [...(this.response?.relations ?? []), ...next.relations],
      };
      this.selected ??= next.relations[0];
      this.relationEmitter.fire();
      this.siteEmitter.fire();
    } catch (error) {
      if (requestSerial === this.requestSerial) {
        void vscode.window.showWarningMessage(
          `FossilSense call relation continuation failed: ${String(error)}`,
        );
      }
    }
  }

  async switchDirection(direction: RelationDirection): Promise<void> {
    await this.show(direction, this.lastLocation === undefined);
  }
}

export function registerCallRelationViews(
  context: vscode.ExtensionContext,
  getClient: () => LanguageClient | undefined,
): CallRelationsController {
  const controller = new CallRelationsController(getClient);
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider(
      'fossilsense.callRelations',
      controller.relationProvider,
    ),
    vscode.window.registerTreeDataProvider('fossilsense.callSites', controller.siteProvider),
    vscode.commands.registerCommand(SHOW_CALL_RELATIONS_COMMAND, () =>
      controller.show('incoming'),
    ),
    vscode.commands.registerCommand(SHOW_INCOMING_CALLS_COMMAND, () =>
      controller.switchDirection('incoming'),
    ),
    vscode.commands.registerCommand(SHOW_OUTGOING_CALLS_COMMAND, () =>
      controller.switchDirection('outgoing'),
    ),
    vscode.commands.registerCommand(REFRESH_CALL_RELATIONS_COMMAND, () => controller.refresh()),
    vscode.commands.registerCommand(LOAD_MORE_CALL_RELATIONS_COMMAND, () => controller.loadMore()),
    vscode.commands.registerCommand(SELECT_CALL_RELATION_COMMAND, (relation: CallRelation) =>
      controller.select(relation),
    ),
    vscode.commands.registerCommand(
      OPEN_CALL_SITE_COMMAND,
      (site: CallSiteFact, workspaceFolder: vscode.WorkspaceFolder | undefined) =>
        openCallSite(site, workspaceFolder),
    ),
  );
  return controller;
}

export async function openCallSite(
  site: CallSiteFact,
  workspaceFolder: vscode.WorkspaceFolder | undefined,
): Promise<void> {
  await openSourceRange(site.path, site.calleeRange, workspaceFolder);
}

async function openSourceRange(
  sourcePath: string,
  sourceRange: CallSiteFact['calleeRange'],
  workspaceFolder: vscode.WorkspaceFolder | undefined,
): Promise<void> {
  const uri = sourceUri(sourcePath, workspaceFolder);
  const document = await vscode.workspace.openTextDocument(uri);
  const editor = await vscode.window.showTextDocument(document);
  const range = new vscode.Range(
    sourceRange.start.line,
    sourceRange.start.character,
    sourceRange.end.line,
    sourceRange.end.character,
  );
  editor.selection = new vscode.Selection(range.start, range.end);
  editor.revealRange(range, vscode.TextEditorRevealType.InCenterIfOutsideViewport);
}

function sourceUri(sourcePath: string, workspaceFolder: vscode.WorkspaceFolder | undefined): vscode.Uri {
  if (path.isAbsolute(sourcePath)) {
    return vscode.Uri.file(sourcePath);
  }
  return workspaceFolder
    ? vscode.Uri.joinPath(workspaceFolder.uri, ...sourcePath.split('/'))
    : vscode.Uri.file(sourcePath);
}

function displayPath(sourcePath: string, workspaceFolder: vscode.WorkspaceFolder | undefined): string {
  if (!path.isAbsolute(sourcePath)) {
    return sourcePath;
  }
  return workspaceFolder ? vscode.workspace.asRelativePath(sourceUri(sourcePath, workspaceFolder), false) : sourcePath;
}
