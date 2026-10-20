import { useLayoutEffect, useReducer, useRef } from 'preact/hooks';

/** Minimal global store: one state object, shallow-merged updates. */
export function createStore(initial) {
  let state = initial;
  const subs = new Set();
  return {
    get: () => state,
    set(patch) {
      const next = typeof patch === 'function' ? patch(state) : patch;
      if (!next) return;
      state = { ...state, ...next };
      for (const f of subs) f();
    },
    subscribe(f) {
      subs.add(f);
      return () => subs.delete(f);
    },
  };
}

/** Re-renders the component when the selected slice changes (Object.is). */
export function useStore(store, select) {
  const [, force] = useReducer((n) => n + 1, 0);
  const sel = useRef(select);
  sel.current = select;
  const value = select(store.get());
  const last = useRef(value);
  last.current = value;
  // Subscribe right after mount and catch up on anything that changed between
  // render and subscription (boot can settle several updates in one tick).
  useLayoutEffect(() => {
    const check = () => {
      if (!Object.is(sel.current(store.get()), last.current)) force();
    };
    check();
    return store.subscribe(check);
  }, []);
  return value;
}
