export type UpdateEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished" };
export interface UpdateResource {
  version: string;
  download(onEvent: (event: UpdateEvent) => void): Promise<void>;
  install(): Promise<void>;
  close(): Promise<void>;
}
export interface UpdateDriver {
  check(): Promise<UpdateResource | null>;
  restart(): Promise<void>;
}
export type UpdateState =
  | { phase: "hidden" }
  | { phase: "available" | "downloaded" | "verifying" | "installing" | "restarting" | "installed"; version: string }
  | { phase: "downloading"; version: string; received: number; total?: number }
  | { phase: "error"; version: string; retry: "download" | "install" | "restart"; message: string };

export function downloadPercent(state: UpdateState): number | undefined {
  if (state.phase !== "downloading" || !state.total) return undefined;
  return Math.min(99, Math.max(0, Math.floor((state.received / state.total) * 100)));
}

export function createUpdater(
  driver: UpdateDriver,
  changed: (state: UpdateState) => void,
  beforeInstall: () => Promise<void> = async () => {},
) {
  let state: UpdateState = { phase: "hidden" };
  let update: UpdateResource | null = null;
  let busy = false;
  let disposed = false;
  const emit = (next: UpdateState) => {
    state = next;
    if (!disposed) changed(next);
  };
  const close = async (resource: UpdateResource | null) => {
    await resource?.close().catch(() => {});
  };
  return {
    get state() {
      return state;
    },
    async check() {
      if (disposed || busy || update) return;
      busy = true;
      try {
        const found = await driver.check();
        if (disposed) {
          await close(found);
          return;
        }
        update = found;
        emit(found ? { phase: "available", version: found.version } : { phase: "hidden" });
      } catch {
        // An unpublished release/offline check is not evidence that an update exists.
        if (!disposed) emit({ phase: "hidden" });
      } finally {
        busy = false;
      }
    },
    async activate() {
      if (disposed || busy || !update) return;
      const current = update;
      const version = current.version;
      const action =
        state.phase === "error"
          ? state.retry
          : state.phase === "available"
            ? "download"
            : state.phase === "downloaded"
              ? "install"
              : state.phase === "installed"
                ? "restart"
                : null;
      if (!action) return;
      busy = true;
      let retry = action;
      try {
        if (action === "download") {
          let received = 0;
          let total: number | undefined;
          emit({ phase: "downloading", version, received });
          await current.download((event) => {
            if (disposed) return;
            if (event.event === "Started") {
              received = 0;
              const size = event.data.contentLength;
              total = size && Number.isFinite(size) && size > 0 ? size : undefined;
            } else if (event.event === "Progress") {
              if (Number.isFinite(event.data.chunkLength) && event.data.chunkLength > 0)
                received += event.data.chunkLength;
            } else {
              emit({ phase: "verifying", version });
              return;
            }
            emit({ phase: "downloading", version, received, total });
          });
          if (disposed) return;
          // download() resolves only after the native updater verifies the signature.
          emit({ phase: "downloaded", version });
          retry = "install";
        }
        await beforeInstall();
        if (disposed) return;
        if (action !== "restart") {
          emit({ phase: "installing", version });
          await current.install();
        }
        if (disposed) return;
        retry = "restart";
        emit({ phase: "restarting", version });
        await driver.restart();
        if (!disposed) emit({ phase: "installed", version });
      } catch (error) {
        if (!disposed)
          emit({ phase: "error", version, retry, message: error instanceof Error ? error.message : String(error) });
      } finally {
        busy = false;
        if (disposed) {
          await close(current);
          if (update === current) update = null;
        }
      }
    },
    dispose() {
      disposed = true;
      if (!busy) {
        void close(update);
        update = null;
      }
    },
  };
}
