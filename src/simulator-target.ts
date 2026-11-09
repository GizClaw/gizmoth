/** Fence asynchronous simulator opens against later target selections without losing local intent on failure. */
export function createSimulatorTargets() {
  const selections = new Map<string, { generation: symbol; simulatorId?: string; pending: boolean }>();
  const begin = (projectId: string) => {
    const generation = Symbol(projectId);
    selections.set(projectId, { generation, simulatorId: selections.get(projectId)?.simulatorId, pending: true });
    return generation;
  };
  return {
    current: (projectId: string) => selections.get(projectId)?.simulatorId,
    pending: (projectId: string) => selections.get(projectId)?.pending === true,
    generation: (projectId: string) => selections.get(projectId)?.generation,
    begin,
    isCurrent: (projectId: string, generation: symbol) => selections.get(projectId)?.generation === generation,
    clear(projectId: string) {
      selections.set(projectId, { generation: Symbol(projectId), pending: false });
    },
    commit(projectId: string, generation: symbol, simulatorId: string) {
      if (selections.get(projectId)?.generation !== generation) return false;
      selections.set(projectId, { generation, simulatorId, pending: false });
      return true;
    },
  };
}
