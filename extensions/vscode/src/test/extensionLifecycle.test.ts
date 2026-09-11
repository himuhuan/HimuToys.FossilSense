import * as assert from 'assert';
import {
  ServiceInstanceScope,
  ServiceLifecycle,
  ServiceLifecycleState,
  ServiceLifecycleTimers,
} from '../extensionLifecycle';

class Deferred {
  readonly promise: Promise<void>;
  private resolvePromise!: () => void;
  private rejectPromise!: (error: Error) => void;

  constructor() {
    this.promise = new Promise<void>((resolve, reject) => {
      this.resolvePromise = resolve;
      this.rejectPromise = reject;
    });
  }

  resolve(): void {
    this.resolvePromise();
  }

  reject(message: string): void {
    this.rejectPromise(new Error(message));
  }
}

class FakeClient {
  startCalls = 0;
  stopCalls = 0;
  readonly stopTimeouts: Array<number | undefined> = [];

  constructor(
    private readonly starts: Deferred[],
    private readonly stops: Deferred[] = [],
  ) {}

  start(): Promise<void> {
    const result = this.starts[this.startCalls++];
    assert.ok(result, 'unexpected start call');
    return result.promise;
  }

  stop(timeoutMs?: number): Promise<void> {
    this.stopTimeouts.push(timeoutMs);
    const result = this.stops[this.stopCalls++];
    assert.ok(result, 'unexpected stop call');
    return result.promise;
  }
}

class CountingDisposable {
  disposed = false;

  constructor(private readonly onDispose: () => void) {}

  dispose(): void {
    if (!this.disposed) {
      this.disposed = true;
      this.onDispose();
    }
  }
}

class FakeTimers implements ServiceLifecycleTimers {
  private nextId = 1;
  private readonly callbacks = new Map<number, () => void>();

  setTimeout(callback: () => void, _delayMs: number): unknown {
    const id = this.nextId++;
    this.callbacks.set(id, callback);
    return id;
  }

  clearTimeout(handle: unknown): void {
    this.callbacks.delete(handle as number);
  }

  get pending(): number {
    return this.callbacks.size;
  }

  runAll(): void {
    const callbacks = [...this.callbacks.values()];
    this.callbacks.clear();
    for (const callback of callbacks) {
      callback();
    }
  }
}

function resolved(): Deferred {
  const result = new Deferred();
  result.resolve();
  return result;
}

async function flushMicrotasks(): Promise<void> {
  for (let index = 0; index < 8; index += 1) {
    await Promise.resolve();
  }
}

interface Harness {
  owner: ServiceLifecycle<FakeClient>;
  states: Array<{ state: ServiceLifecycleState; error?: unknown }>;
  guardedWrites: Array<(value: string) => void>;
  writes: string[];
  scopes: ServiceInstanceScope[];
  activeResources: () => number;
  createdCount: () => number;
}

function createHarness(
  clients: FakeClient[],
  timers = new FakeTimers(),
  stopTimeoutMs?: number,
  stopFailureRecoveryDelayMs?: number,
): Harness {
  const states: Array<{ state: ServiceLifecycleState; error?: unknown }> = [];
  const guardedWrites: Array<(value: string) => void> = [];
  const writes: string[] = [];
  const scopes: ServiceInstanceScope[] = [];
  let resources = 0;
  let created = 0;

  const owner = new ServiceLifecycle<FakeClient>({
    create(scope: ServiceInstanceScope) {
      const client = clients[created++];
      assert.ok(client, 'unexpected service creation');
      scopes.push(scope);
      resources += 1;
      scope.own(new CountingDisposable(() => resources -= 1));
      guardedWrites.push(scope.guard((value: string) => writes.push(value)));
      return { client };
    },
    stateChanged(state, error) {
      states.push({ state, error });
    },
    timers,
    stopTimeoutMs,
    stopFailureRecoveryDelayMs,
  });

  return {
    owner,
    states,
    guardedWrites,
    writes,
    scopes,
    activeResources: () => resources,
    createdCount: () => created,
  };
}

async function main(): Promise<void> {
  {
    const oldStart = new Deferred();
    const newStart = new Deferred();
    const oldClient = new FakeClient([oldStart]);
    const newClient = new FakeClient([newStart]);
    const harness = createHarness([oldClient, newClient]);

    const oldOperation = harness.owner.start();
    assert.strictEqual(harness.owner.start(), oldOperation, 'duplicate start created new work');
    const restart = harness.owner.restart();
    assert.strictEqual(harness.createdCount(), 2);
    assert.strictEqual(harness.activeResources(), 1);

    newStart.resolve();
    await restart;
    oldStart.reject('old startup failed late');
    await oldOperation;

    assert.strictEqual(harness.owner.client, newClient);
    assert.strictEqual(harness.states.at(-1)?.state, 'ready');
    assert.strictEqual(harness.activeResources(), 1);
    harness.guardedWrites[0]('old-late');
    harness.guardedWrites[1]('new');
    assert.deepStrictEqual(harness.writes, ['new']);
  }

  {
    const oldStop = new Deferred();
    const oldClient = new FakeClient([resolved()], [oldStop]);
    const newClient = new FakeClient([resolved()]);
    const harness = createHarness([oldClient, newClient]);

    await harness.owner.start();
    const restart = harness.owner.restart();
    assert.strictEqual(harness.createdCount(), 1, 'restart must wait for confirmed stop');
    harness.guardedWrites[0]('old');
    assert.deepStrictEqual(harness.writes, []);

    oldStop.resolve();
    await restart;
    assert.strictEqual(harness.createdCount(), 2);
    assert.strictEqual(harness.owner.client, newClient);
    harness.guardedWrites[0]('old-late');
    harness.guardedWrites[1]('new');
    assert.deepStrictEqual(harness.writes, ['new']);
  }

  {
    const timers = new FakeTimers();
    const client = new FakeClient([resolved()], [resolved()]);
    const harness = createHarness([client], timers, 5000);

    await harness.owner.start();
    harness.owner.scheduleRestart(150);
    assert.strictEqual(timers.pending, 1);
    await harness.owner.stop();
    assert.deepStrictEqual(client.stopTimeouts, [5000]);
    assert.strictEqual(timers.pending, 0);
    timers.runAll();
    assert.strictEqual(harness.createdCount(), 1);
    assert.strictEqual(harness.owner.client, undefined);
    assert.strictEqual(harness.states.at(-1)?.state, 'stopped');
  }

  {
    const lateStart = new Deferred();
    const client = new FakeClient([lateStart], [resolved()]);
    const harness = createHarness([client]);

    const startup = harness.owner.start();
    await harness.owner.stop();
    assert.strictEqual(client.stopCalls, 0, 'an unready client was stopped directly');
    assert.strictEqual(harness.states.at(-1)?.state, 'stopped');

    lateStart.resolve();
    await startup;
    await flushMicrotasks();
    assert.strictEqual(client.stopCalls, 1, 'a superseded client was not stopped after startup');
    assert.strictEqual(harness.states.at(-1)?.state, 'stopped');
    assert.strictEqual(harness.owner.client, undefined);
  }

  {
    const timers = new FakeTimers();
    const lateStart = new Deferred();
    const lateStop = new Deferred();
    const retiredClient = new FakeClient([lateStart], [lateStop, resolved()]);
    const replacement = new FakeClient([resolved()]);
    const harness = createHarness(
      [retiredClient, replacement],
      timers,
      undefined,
      2500,
    );

    const startup = harness.owner.start();
    await harness.owner.stop();
    lateStart.resolve();
    await startup;
    assert.strictEqual(retiredClient.stopCalls, 1);
    lateStop.reject('retired stop refused');
    await flushMicrotasks();
    assert.strictEqual(harness.owner.hasUnconfirmedStop, true);
    assert.strictEqual(harness.states.at(-1)?.state, 'stopFailed');

    const firstStart = harness.owner.start();
    await flushMicrotasks();
    assert.strictEqual(retiredClient.stopCalls, 2);
    assert.strictEqual(harness.createdCount(), 1);
    assert.strictEqual(timers.pending, 1);

    const secondStart = harness.owner.start();
    const stopDuringRecovery = harness.owner.stop();
    assert.strictEqual(harness.createdCount(), 1);
    assert.notStrictEqual(harness.states.at(-1)?.state, 'stopped');

    timers.runAll();
    await Promise.all([firstStart, secondStart, stopDuringRecovery]);
    assert.strictEqual(harness.owner.hasUnconfirmedStop, false);
    assert.strictEqual(harness.states.at(-1)?.state, 'stopped');
    assert.strictEqual(harness.createdCount(), 1);
    await harness.owner.start();
    assert.strictEqual(harness.owner.client, replacement);
  }

  {
    const timers = new FakeTimers();
    const oldClient = new FakeClient([resolved()], [resolved()]);
    const newClient = new FakeClient([resolved()]);
    const harness = createHarness([oldClient, newClient], timers);

    await harness.owner.start();
    harness.owner.scheduleRestart(150);
    harness.owner.scheduleRestart(150);
    assert.strictEqual(timers.pending, 1);
    timers.runAll();
    await flushMicrotasks();
    assert.strictEqual(harness.createdCount(), 2);
    assert.strictEqual(harness.owner.client, newClient);
    assert.strictEqual(harness.activeResources(), 1);
  }

  {
    const timers = new FakeTimers();
    const failedClient = new FakeClient([resolved()]);
    const retryClient = new FakeClient([resolved()]);
    const harness = createHarness([failedClient, retryClient], timers);

    await harness.owner.start();
    harness.owner.scheduleRestart(150);
    assert.strictEqual(timers.pending, 1);
    harness.scopes[0].fail(new Error('connection closed'));
    assert.strictEqual(timers.pending, 0);
    assert.strictEqual(harness.owner.client, undefined);
    assert.strictEqual(harness.activeResources(), 0);
    assert.strictEqual(harness.states.at(-1)?.state, 'failed');

    harness.scopes[0].fail(new Error('old close repeated'));
    assert.strictEqual(harness.states.at(-1)?.error?.toString(), 'Error: connection closed');
    timers.runAll();
    await flushMicrotasks();
    assert.strictEqual(harness.createdCount(), 1);
    await harness.owner.start();
    assert.strictEqual(harness.owner.client, retryClient);
  }

  {
    const states: Array<{ state: ServiceLifecycleState; error?: unknown }> = [];
    const client = new FakeClient([resolved()]);
    let disposed = false;
    const owner = new ServiceLifecycle<FakeClient>({
      create(scope) {
        scope.own(new CountingDisposable(() => disposed = true));
        return { client, isReady: () => false };
      },
      stateChanged(state, error) {
        states.push({ state, error });
      },
    });

    await owner.start();
    assert.strictEqual(owner.client, undefined);
    assert.strictEqual(disposed, true);
    assert.strictEqual(states.at(-1)?.state, 'startFailed');
  }

  {
    const failedStart = new Deferred();
    const failedClient = new FakeClient([failedStart]);
    const retryClient = new FakeClient([resolved()], [resolved()]);
    const finalClient = new FakeClient([resolved()]);
    const harness = createHarness([failedClient, retryClient, finalClient]);

    const failedOperation = harness.owner.start();
    failedStart.reject('startup failed');
    await failedOperation;
    assert.strictEqual(harness.states.at(-1)?.state, 'startFailed');
    assert.strictEqual(harness.activeResources(), 0);

    await harness.owner.start();
    assert.strictEqual(harness.owner.client, retryClient);
    assert.strictEqual(harness.activeResources(), 1);
    await harness.owner.restart();
    assert.strictEqual(harness.owner.client, finalClient);
    assert.strictEqual(harness.activeResources(), 1, 'restart accumulated instance resources');
  }

  {
    const timers = new FakeTimers();
    const failedStop = new Deferred();
    const retryStop = resolved();
    const blockedClient = new FakeClient([resolved()], [failedStop, retryStop]);
    const replacement = new FakeClient([resolved()]);
    const harness = createHarness([blockedClient, replacement], timers, undefined, 2500);

    await harness.owner.start();
    const failedStopOperation = harness.owner.stop();
    failedStop.reject('stop refused');
    await failedStopOperation;
    assert.strictEqual(harness.states.at(-1)?.state, 'stopFailed');
    assert.strictEqual(harness.owner.hasUnconfirmedStop, true);
    assert.strictEqual(harness.owner.client, undefined);
    assert.strictEqual(harness.activeResources(), 0);
    assert.strictEqual(harness.createdCount(), 1);

    const retry = harness.owner.start();
    assert.strictEqual(blockedClient.stopCalls, 2);
    assert.strictEqual(harness.createdCount(), 1);
    await flushMicrotasks();
    assert.strictEqual(timers.pending, 1);
    assert.strictEqual(harness.owner.hasUnconfirmedStop, true);
    timers.runAll();
    await retry;
    assert.strictEqual(harness.createdCount(), 2);
    assert.strictEqual(harness.owner.client, replacement);
    assert.strictEqual(harness.owner.hasUnconfirmedStop, false);
  }

  {
    const timers = new FakeTimers();
    const client = new FakeClient([resolved()], [resolved()]);
    const harness = createHarness([client], timers);

    await harness.owner.start();
    harness.owner.scheduleRestart(150);
    await harness.owner.deactivate();
    timers.runAll();
    await harness.owner.start();
    assert.strictEqual(harness.createdCount(), 1);
    assert.strictEqual(harness.states.at(-1)?.state, 'deactivated');
  }
}

void main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
