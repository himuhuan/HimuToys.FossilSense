export interface DisposableLike {
  dispose(): unknown;
}

export interface ServiceLifecycleClient {
  start(): Promise<void>;
  stop(timeoutMs?: number): Promise<void>;
}

export type ServiceLifecycleState =
  | 'starting'
  | 'ready'
  | 'stopping'
  | 'stopped'
  | 'startFailed'
  | 'failed'
  | 'stopFailed'
  | 'deactivated';

export interface ServiceLifecycleTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface ServiceInstanceScope {
  readonly id: number;
  isCurrent(): boolean;
  fail(error: unknown): void;
  own<T extends DisposableLike>(resource: T): T;
  guard<TArguments extends unknown[]>(
    callback: (...args: TArguments) => void,
  ): (...args: TArguments) => void;
}

export interface CreatedService<C extends ServiceLifecycleClient> {
  client: C;
  isReady?: () => boolean;
  onReady?: (scope: ServiceInstanceScope) => void | Promise<void>;
}

export interface ServiceLifecycleOptions<C extends ServiceLifecycleClient> {
  create(scope: ServiceInstanceScope): CreatedService<C> | undefined;
  stateChanged(state: ServiceLifecycleState, error?: unknown): void;
  backgroundError?(error: unknown): void;
  timers?: ServiceLifecycleTimers;
  stopTimeoutMs?: number;
  stopFailureRecoveryDelayMs?: number;
}

type ServiceIntent = 'running' | 'stopped' | 'deactivated';
type ServicePhase =
  | 'creating'
  | 'starting'
  | 'ready'
  | 'stopping'
  | 'stopFailed'
  | 'stopped'
  | 'startFailed'
  | 'failed';

interface ServiceRecord<C extends ServiceLifecycleClient> {
  readonly id: number;
  readonly resources: DisposableLike[];
  readonly scope: ServiceInstanceScope;
  authoritative: boolean;
  resourcesDisposed: boolean;
  phase: ServicePhase;
  created?: CreatedService<C>;
  startPromise?: Promise<void>;
  stopPromise?: Promise<boolean>;
  stopError?: unknown;
  requiresStopRecoveryDelay?: boolean;
}

const nativeTimers: ServiceLifecycleTimers = {
  setTimeout(callback, delayMs) {
    return setTimeout(callback, delayMs);
  },
  clearTimeout(handle) {
    clearTimeout(handle as ReturnType<typeof setTimeout>);
  },
};

export class ServiceLifecycle<C extends ServiceLifecycleClient> {
  private current: ServiceRecord<C> | undefined;
  private readonly retired = new Set<ServiceRecord<C>>();
  private intent: ServiceIntent = 'stopped';
  private nextId = 1;
  private restartTimer: unknown;

  constructor(private readonly options: ServiceLifecycleOptions<C>) {}

  get client(): C | undefined {
    if (!this.current?.authoritative) {
      return undefined;
    }
    return this.current.created?.client;
  }

  get hasUnconfirmedStop(): boolean {
    if (
      this.current?.phase === 'stopFailed' ||
      this.current?.stopPromise !== undefined ||
      this.current?.requiresStopRecoveryDelay
    ) {
      return true;
    }
    return [...this.retired].some(
      (record) =>
        record.phase === 'stopFailed' ||
        record.stopPromise !== undefined ||
        record.requiresStopRecoveryDelay,
    );
  }

  isCurrentClient(client: C): boolean {
    return this.client === client;
  }

  start(): Promise<void> {
    if (this.intent === 'deactivated') {
      return Promise.resolve();
    }

    this.cancelScheduledRestart();
    this.intent = 'running';
    if (this.hasRetiredStopWork()) {
      return this.retryRetiredThen(() => this.startCurrent());
    }
    return this.startCurrent();
  }

  private startCurrent(): Promise<void> {
    if (this.intent !== 'running') {
      return Promise.resolve();
    }
    const record = this.current;
    if (!record) {
      return this.createCurrent();
    }
    if (record.authoritative && (record.phase === 'starting' || record.phase === 'ready')) {
      return record.startPromise ?? Promise.resolve();
    }
    return this.stopThenStartIfRequested(record);
  }

  restart(): Promise<void> {
    if (this.intent === 'deactivated') {
      return Promise.resolve();
    }

    this.cancelScheduledRestart();
    this.intent = 'running';
    if (this.hasRetiredStopWork()) {
      return this.retryRetiredThen(() => this.restartCurrent());
    }
    return this.restartCurrent();
  }

  private restartCurrent(): Promise<void> {
    if (this.intent !== 'running') {
      return Promise.resolve();
    }
    const record = this.current;
    if (!record) {
      return this.createCurrent();
    }
    if (record.authoritative && record.phase === 'starting') {
      this.retireStarting(record);
      return this.createCurrent();
    }
    return this.stopThenStartIfRequested(record);
  }

  async stop(): Promise<void> {
    if (this.intent === 'deactivated') {
      return;
    }

    this.cancelScheduledRestart();
    this.intent = 'stopped';
    let stopped = true;
    const record = this.current;
    if (record) {
      if (record.authoritative && record.phase === 'starting') {
        this.retireStarting(record);
      } else {
        stopped = await this.stopRecord(record, true);
      }
    }

    const retiredStopped = await this.retryRetiredStops();
    if (stopped && retiredStopped && this.intent === 'stopped') {
      this.options.stateChanged('stopped');
    } else if (!retiredStopped && !this.current) {
      this.options.stateChanged('stopFailed', this.retiredStopError());
    }
  }

  async deactivate(): Promise<void> {
    if (this.intent === 'deactivated') {
      return;
    }

    this.cancelScheduledRestart();
    this.intent = 'deactivated';
    let stopped = true;
    const record = this.current;
    if (record) {
      if (record.authoritative && record.phase === 'starting') {
        this.retireStarting(record);
      } else {
        stopped = await this.stopRecord(record, true);
      }
    }

    const retiredStopped = await this.retryRetiredStops();
    if (stopped && retiredStopped) {
      this.options.stateChanged('deactivated');
    } else if (!retiredStopped && !this.current) {
      this.options.stateChanged('stopFailed', this.retiredStopError());
    }
  }

  scheduleRestart(delayMs: number): void {
    if (this.intent !== 'running' || !this.current?.authoritative) {
      return;
    }

    this.cancelScheduledRestart();
    const timers = this.options.timers ?? nativeTimers;
    const handle = timers.setTimeout(() => {
      if (this.restartTimer !== handle || this.intent !== 'running') {
        return;
      }
      this.restartTimer = undefined;
      void this.restart();
    }, delayMs);
    this.restartTimer = handle;
  }

  private createCurrent(): Promise<void> {
    if (this.intent !== 'running') {
      return Promise.resolve();
    }
    if (this.current) {
      return this.current.startPromise ?? Promise.resolve();
    }

    const record = this.newRecord();
    this.current = record;
    try {
      record.created = this.options.create(record.scope);
    } catch (error) {
      this.handleStartFailure(record, error);
      return Promise.resolve();
    }

    if (!record.created) {
      record.authoritative = false;
      this.current = undefined;
      this.disposeResources(record);
      return Promise.resolve();
    }

    record.phase = 'starting';
    this.options.stateChanged('starting');
    let start: Promise<void>;
    try {
      start = record.created.client.start();
    } catch (error) {
      this.handleStartFailure(record, error);
      return Promise.resolve();
    }

    record.startPromise = Promise.resolve(start).then(
      () => this.handleStartSuccess(record),
      (error) => this.handleStartFailure(record, error),
    );
    return record.startPromise;
  }

  private newRecord(): ServiceRecord<C> {
    let record: ServiceRecord<C>;
    const scope: ServiceInstanceScope = {
      id: this.nextId++,
      isCurrent: () => this.isCurrent(record),
      fail: (error: unknown): void => this.handleServiceFailure(record, error),
      own: <T extends DisposableLike>(resource: T): T => {
        if (record.resourcesDisposed) {
          this.disposeOne(resource);
        } else {
          record.resources.push(resource);
        }
        return resource;
      },
      guard: <TArguments extends unknown[]>(
        callback: (...args: TArguments) => void,
      ): ((...args: TArguments) => void) => (...args: TArguments): void => {
        if (this.isCurrent(record)) {
          callback(...args);
        }
      },
    };
    record = {
      id: scope.id,
      resources: [],
      authoritative: true,
      resourcesDisposed: false,
      phase: 'creating',
      scope,
    };
    return record;
  }

  private async handleStartSuccess(record: ServiceRecord<C>): Promise<void> {
    try {
      if (record.created?.isReady && !record.created.isReady()) {
        this.handleStartFailure(record, new Error('service did not reach the ready state'));
        return;
      }
    } catch (error) {
      this.handleStartFailure(record, error);
      return;
    }
    record.phase = 'ready';
    if (!this.isCurrent(record) || this.intent !== 'running') {
      void this.stopRetired(record);
      return;
    }

    this.options.stateChanged('ready');
    if (!record.created?.onReady) {
      return;
    }
    try {
      await record.created.onReady(record.scope);
    } catch (error) {
      if (this.isCurrent(record)) {
        this.reportBackgroundError(error);
      }
    }
  }

  private handleStartFailure(record: ServiceRecord<C>, error: unknown): void {
    const wasCurrent = this.current === record;
    if (wasCurrent) {
      this.current = undefined;
    }
    record.authoritative = false;
    record.phase = 'startFailed';
    this.retired.delete(record);
    this.disposeResources(record);
    if (wasCurrent && this.intent === 'running') {
      this.options.stateChanged('startFailed', error);
    }
  }

  private handleServiceFailure(record: ServiceRecord<C>, error: unknown): void {
    if (!this.isCurrent(record)) {
      return;
    }
    this.current = undefined;
    this.cancelScheduledRestart();
    this.intent = 'stopped';
    record.authoritative = false;
    record.phase = 'failed';
    this.disposeResources(record);
    this.options.stateChanged('failed', error);
  }

  private retireStarting(record: ServiceRecord<C>): void {
    if (this.current === record) {
      this.current = undefined;
    }
    record.authoritative = false;
    this.retired.add(record);
    this.disposeResources(record);
  }

  private async stopThenStartIfRequested(record: ServiceRecord<C>): Promise<void> {
    const stopped = await this.stopRecord(record, true);
    if (stopped && this.intent === 'running') {
      await this.createCurrent();
    }
  }

  private hasRetiredStopWork(): boolean {
    return [...this.retired].some(
      (record) =>
        record.phase === 'stopFailed' ||
        record.stopPromise !== undefined ||
        record.requiresStopRecoveryDelay,
    );
  }

  private async retryRetiredThen(action: () => Promise<void>): Promise<void> {
    const stopped = await this.retryRetiredStops();
    if (!stopped) {
      if (!this.current) {
        this.options.stateChanged('stopFailed', this.retiredStopError());
      }
      return;
    }
    if (this.intent === 'running') {
      await action();
    }
  }

  private async retryRetiredStops(): Promise<boolean> {
    const pending = [...this.retired].filter(
      (record) => record.phase === 'stopFailed' || record.stopPromise !== undefined,
    );
    if (pending.length === 0) {
      return true;
    }
    const results = await Promise.all(
      pending.map((record) => record.stopPromise ?? this.stopRecord(record, false)),
    );
    return results.every(Boolean);
  }

  private retiredStopError(): unknown {
    return [...this.retired].find((record) => record.phase === 'stopFailed')?.stopError;
  }

  private stopRecord(record: ServiceRecord<C>, observable: boolean): Promise<boolean> {
    if (record.stopPromise) {
      return record.stopPromise;
    }
    if (!record.created) {
      return Promise.resolve(true);
    }

    record.authoritative = false;
    this.disposeResources(record);
    record.phase = 'stopping';
    if (observable && this.current === record) {
      this.options.stateChanged('stopping');
    }

    let stop: Promise<void>;
    try {
      stop = record.created.client.stop(this.options.stopTimeoutMs);
    } catch (error) {
      this.handleStopFailure(record, error, observable);
      return Promise.resolve(false);
    }

    record.stopPromise = Promise.resolve(stop).then(
      async () => {
        if (
          record.requiresStopRecoveryDelay &&
          (this.options.stopFailureRecoveryDelayMs ?? 0) > 0
        ) {
          await this.delay(this.options.stopFailureRecoveryDelayMs ?? 0);
        }
        record.phase = 'stopped';
        record.stopPromise = undefined;
        record.stopError = undefined;
        record.requiresStopRecoveryDelay = false;
        this.retired.delete(record);
        if (this.current === record) {
          this.current = undefined;
        }
        return true;
      },
      (error) => {
        this.handleStopFailure(record, error, observable);
        return false;
      },
    );
    return record.stopPromise;
  }

  private handleStopFailure(
    record: ServiceRecord<C>,
    error: unknown,
    observable: boolean,
  ): void {
    record.phase = 'stopFailed';
    record.stopPromise = undefined;
    record.stopError = error;
    record.requiresStopRecoveryDelay = true;
    if (this.current !== record) {
      this.retired.add(record);
    }
    if (observable && this.current === record) {
      this.options.stateChanged('stopFailed', error);
    } else if (!this.current) {
      this.options.stateChanged('stopFailed', error);
    } else {
      this.reportBackgroundError(error);
    }
  }

  private async stopRetired(record: ServiceRecord<C>): Promise<void> {
    this.retired.add(record);
    await this.stopRecord(record, false);
  }

  private isCurrent(record: ServiceRecord<C>): boolean {
    return this.current === record && record.authoritative;
  }

  private delay(delayMs: number): Promise<void> {
    return new Promise((resolve) => {
      (this.options.timers ?? nativeTimers).setTimeout(resolve, delayMs);
    });
  }

  private cancelScheduledRestart(): void {
    if (this.restartTimer === undefined) {
      return;
    }
    (this.options.timers ?? nativeTimers).clearTimeout(this.restartTimer);
    this.restartTimer = undefined;
  }

  private disposeResources(record: ServiceRecord<C>): void {
    if (record.resourcesDisposed) {
      return;
    }
    record.resourcesDisposed = true;
    for (const resource of record.resources.splice(0).reverse()) {
      this.disposeOne(resource);
    }
  }

  private disposeOne(resource: DisposableLike): void {
    try {
      resource.dispose();
    } catch (error) {
      this.reportBackgroundError(error);
    }
  }

  private reportBackgroundError(error: unknown): void {
    this.options.backgroundError?.(error);
  }
}
