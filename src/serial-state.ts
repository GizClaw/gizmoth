import type { LogEntry, LogPage, SerialSnapshot } from "./serial";

/** Persistent daemon generations order processes; revisions order one process.
 * Request order only prevents an old response from undoing an offline read. */
export class SerialSnapshotOrder {
  private instance: string | null = null;
  private generation = 0;
  private version = -1;
  private request = 0;
  private acceptedRequest = 0;
  private offlineRequest = 0;

  begin(): number {
    return ++this.request;
  }

  accepts(next: SerialSnapshot, request: number): boolean {
    if (request < this.offlineRequest) return false;
    if (!next.running) {
      if (request < this.acceptedRequest) return false;
      this.offlineRequest = request;
      this.acceptedRequest = request;
      return true;
    }
    const generation = next.instanceGeneration;
    if (!Number.isSafeInteger(generation) || generation < 0 || generation < this.generation) return false;
    const instance = next.instanceId ?? null;
    if (generation === 0) {
      // Legacy daemons have no authoritative generation. They remain readable
      // only until a modern daemon has been discovered.
      if (instance !== this.instance && request < this.acceptedRequest) return false;
      if (instance === this.instance && next.version < this.version) return false;
    } else {
      if (!instance) return false;
      if (generation === this.generation && (instance !== this.instance || next.version < this.version)) return false;
    }
    this.instance = instance;
    this.generation = generation;
    this.version = next.version;
    this.acceptedRequest = Math.max(this.acceptedRequest, request);
    return true;
  }
}

export interface SerialLogTarget {
  id: string;
  instanceId?: string | null;
  generation?: number;
  seq?: number;
  running: boolean;
}

export interface SerialLogState {
  entries: LogEntry[];
  error: string;
}

type ReadPage = (id: string, after?: number, limit?: number) => Promise<LogPage>;
const RETAIN = 3000;

/** One in-flight read per mounted log. New watermarks request a follow-up read;
 * only a port/process/log-generation change invalidates a response. */
export class SerialLogReader {
  private target: SerialLogTarget | null = null;
  private instance: string | null = null;
  private generation = 0;
  private cursor: number | null = null;
  private token = 0;
  private inFlight = false;
  private listeners = new Set<() => void>();
  private retry: ReturnType<typeof setTimeout> | undefined;
  private value: SerialLogState = { entries: [], error: "" };

  private readonly load: ReadPage;

  constructor(load: ReadPage) {
    this.load = load;
  }

  read = (): SerialLogState => this.value;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    void this.pump();
    return () => {
      this.listeners.delete(listener);
      if (!this.listeners.size) {
        this.token++;
        this.inFlight = false;
        this.cancelRetry();
      }
    };
  };

  update(next: SerialLogTarget) {
    const instance = next.instanceId ?? null;
    const generation = next.generation ?? 0;
    const differentPort = this.target?.id !== next.id;
    const differentInstance = next.running && this.instance !== instance;
    if (differentPort || differentInstance) {
      this.instance = instance;
      this.generation = generation;
      this.target = next;
      this.invalidate();
    } else if (next.running && generation > this.generation) {
      this.generation = generation;
      this.target = next;
      this.invalidate();
    } else if (next.running && generation < this.generation) {
      // A log page may observe a clear before the next overview does.
      this.target = { ...this.target!, running: next.running };
    } else {
      this.target = {
        ...next,
        seq: Math.max(next.seq ?? 0, this.target?.seq ?? 0),
      };
    }
    void this.pump();
  }

  reset = () => {
    this.invalidate();
    void this.pump();
  };

  private cancelRetry() {
    if (this.retry !== undefined) clearTimeout(this.retry);
    this.retry = undefined;
  }

  private invalidate() {
    this.token++;
    this.inFlight = false;
    this.cursor = null;
    this.cancelRetry();
    this.publish([], "");
  }

  private publish(entries: LogEntry[], error: string) {
    this.value = { entries, error };
    for (const listener of this.listeners) listener();
  }

  private retryLater() {
    if (!this.listeners.size || !this.target?.running || this.retry !== undefined) return;
    this.retry = setTimeout(() => {
      this.retry = undefined;
      void this.pump();
    }, 1000);
  }

  private async pump(): Promise<void> {
    const target = this.target;
    if (!target?.running || !this.listeners.size || this.inFlight || this.retry !== undefined) return;
    if (this.cursor !== null && (target.seq ?? 0) <= this.cursor) return;
    const token = this.token;
    const after = this.cursor;
    this.inFlight = true;
    let retry = false;
    let progressed = false;
    try {
      const page = await this.load(target.id, after ?? undefined, after === null ? 600 : 1000);
      if (token !== this.token || !this.listeners.size) return;
      if (page.instanceId && this.instance && page.instanceId !== this.instance) {
        // Wait for discovery to publish the replacement process before showing
        // its logs alongside the previous process's state.
        retry = true;
        return;
      }
      const generation = page.generation ?? this.generation;
      if (generation > this.generation) {
        this.generation = generation;
        this.target = { ...this.target!, generation, seq: page.next };
        this.invalidate();
        void this.pump();
        return;
      }
      if (generation < this.generation) {
        retry = true;
        return;
      }
      const fresh = after === null ? page.entries : page.entries.filter((entry) => entry.seq > after);
      const entries = after === null || page.truncated ? fresh : [...this.value.entries, ...fresh];
      this.cursor = Math.max(after ?? 0, page.next);
      progressed = after === null || this.cursor > after;
      if (fresh.length || after === null || page.truncated || this.value.error) {
        this.publish(entries.length > RETAIN ? entries.slice(-RETAIN) : entries, "");
      }
      // A captured overview can already be stale. An empty/no-progress page must
      // not turn that watermark into an immediate request loop.
      if (!progressed && (this.target?.seq ?? 0) > this.cursor) retry = true;
    } catch (error) {
      if (token !== this.token || !this.listeners.size) return;
      this.publish(this.value.entries, error instanceof Error ? error.message : String(error));
      retry = true;
    } finally {
      if (token === this.token) {
        this.inFlight = false;
        if (retry) this.retryLater();
        else if (progressed) void this.pump();
      }
    }
  }
}
