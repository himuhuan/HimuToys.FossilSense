import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import {
  CLEAR_COMPLETION_HISTORY_COMMAND,
  CLEAR_COMPLETION_HISTORY_LSP_COMMAND,
  completionHistoryInitializationOptions,
  clearCompletionHistoryRequest,
} from '../completionHistory';

assert.deepStrictEqual(completionHistoryInitializationOptions('auto'), {
  completionHistory: { mode: 'auto' },
});
assert.deepStrictEqual(completionHistoryInitializationOptions('on'), {
  completionHistory: { mode: 'on' },
});
assert.deepStrictEqual(completionHistoryInitializationOptions('off'), {
  completionHistory: { mode: 'off' },
});
assert.deepStrictEqual(completionHistoryInitializationOptions('unexpected'), {
  completionHistory: { mode: 'auto' },
});

assert.deepStrictEqual(clearCompletionHistoryRequest(), {
  command: CLEAR_COMPLETION_HISTORY_LSP_COMMAND,
  arguments: [],
});

const packageJson = JSON.parse(
  fs.readFileSync(path.resolve(__dirname, '..', '..', 'package.json'), 'utf8'),
);

assert.strictEqual(packageJson.version, '1.3.3');
assert.ok(
  packageJson.contributes.commands.some(
    (command: { command: string }) => command.command === CLEAR_COMPLETION_HISTORY_COMMAND,
  ),
);
assert.strictEqual(
  packageJson.contributes.configuration.properties['fossilsense.completionHistory.mode']
    .default,
  'auto',
);
