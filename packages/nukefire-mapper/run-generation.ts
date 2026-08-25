/** Whether asynchronous mapper work still belongs to the active ownership run. */
export function isCurrentMapperRun(
  started: boolean,
  currentGeneration: number,
  capturedGeneration: number,
): boolean {
  return started && currentGeneration === capturedGeneration;
}

export class ObsoleteNukeFireMapperRunError extends Error {
  constructor() {
    super("NukeFire mapper ownership changed while processing a snapshot");
    this.name = "ObsoleteNukeFireMapperRunError";
  }
}

export function assertCurrentMapperRun(
  started: boolean,
  currentGeneration: number,
  capturedGeneration: number,
): void {
  if (!isCurrentMapperRun(started, currentGeneration, capturedGeneration)) {
    throw new ObsoleteNukeFireMapperRunError();
  }
}

/** Run one async phase and reject its result if ownership changed at any point. */
export async function whileCurrentMapperRun<T>(
  capturedGeneration: number,
  current: () => { started: boolean; generation: number },
  operation: () => Promise<T>,
): Promise<T> {
  const assertCurrent = (): void => {
    const state = current();
    assertCurrentMapperRun(state.started, state.generation, capturedGeneration);
  };
  assertCurrent();
  try {
    const result = await operation();
    assertCurrent();
    return result;
  } catch (caught) {
    assertCurrent();
    throw caught;
  }
}
