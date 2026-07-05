import { normalizeOnOffAuto, OnOffAutoMode } from './config';

export const CLEAR_COMPLETION_HISTORY_COMMAND = 'fossilsense.clearCompletionHistory';
export const CLEAR_COMPLETION_HISTORY_LSP_COMMAND = 'fossilsense.lsp.clearCompletionHistory';

export interface CompletionHistoryInitializationOptions {
  completionHistory: {
    mode: OnOffAutoMode;
  };
}

export interface ExecuteCommandRequestPayload {
  command: string;
  arguments: unknown[];
}

export function completionHistoryInitializationOptions(
  mode: string | undefined,
): CompletionHistoryInitializationOptions {
  return {
    completionHistory: {
      mode: normalizeOnOffAuto(mode),
    },
  };
}

export function clearCompletionHistoryRequest(): ExecuteCommandRequestPayload {
  return {
    command: CLEAR_COMPLETION_HISTORY_LSP_COMMAND,
    arguments: [],
  };
}
