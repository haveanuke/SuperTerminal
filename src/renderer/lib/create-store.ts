import { useSyncExternalStore, useRef, useCallback } from 'react';

type SetState<T> = (partial: Partial<T> | ((state: T) => Partial<T>)) => void;
type GetState<T> = () => T;
type StateCreator<T> = (set: SetState<T>, get: GetState<T>) => T;

interface StoreApi<T> {
  getState: GetState<T>;
  setState: SetState<T>;
  subscribe: (listener: () => void) => () => void;
}

function shallowEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  const keysA = Object.keys(a as Record<string, unknown>);
  const keysB = Object.keys(b as Record<string, unknown>);
  if (keysA.length !== keysB.length) return false;
  for (const key of keysA) {
    if (!Object.is(
      (a as Record<string, unknown>)[key],
      (b as Record<string, unknown>)[key]
    )) return false;
  }
  return true;
}

export function createStore<T extends object>(creator: StateCreator<T>) {
  let state: T;
  const listeners = new Set<() => void>();

  const getState: GetState<T> = () => state;

  const setState: SetState<T> = (partial) => {
    const nextPartial = typeof partial === 'function' ? partial(state) : partial;
    const nextState = Object.assign({}, state, nextPartial);
    if (!Object.is(state, nextState)) {
      state = nextState;
      listeners.forEach((listener) => listener());
    }
  };

  const subscribe = (listener: () => void) => {
    listeners.add(listener);
    return () => listeners.delete(listener);
  };

  state = creator(setState, getState);

  const api: StoreApi<T> = { getState, setState, subscribe };

  function useStore(): T;
  function useStore<U>(selector: (state: T) => U): U;
  function useStore<U>(selector?: (state: T) => U): T | U {
    const selectorRef = useRef(selector);
    selectorRef.current = selector;
    const prevRef = useRef<T | U>();

    const getSnapshot = useCallback(() => {
      const next = selectorRef.current ? selectorRef.current(state) : state;
      if (shallowEqual(prevRef.current, next)) return prevRef.current as T | U;
      prevRef.current = next;
      return next;
    }, []);

    return useSyncExternalStore(subscribe, getSnapshot);
  }

  useStore.getState = api.getState;
  useStore.setState = api.setState;
  useStore.subscribe = api.subscribe;

  return useStore;
}
