const preparations = new Set<() => Promise<void>>();

export function beforeAppUpdate(prepare: () => Promise<void>): () => void {
  preparations.add(prepare);
  return () => {
    preparations.delete(prepare);
  };
}

/** Flush the open project and stop installation while a device/agent operation is active. */
export async function prepareAppUpdate() {
  for (const prepare of preparations) await prepare();
}
