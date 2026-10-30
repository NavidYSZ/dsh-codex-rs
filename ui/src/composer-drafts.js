// Unsent content belongs to a draft, even while its chat changes or its
// request is in flight. A submission owns only the text revision and
// attachment keys it captured; its completion cannot erase later work.
function messageId() {
  if (globalThis.crypto.randomUUID) return globalThis.crypto.randomUUID();
  // getRandomValues also works when the UI is served over plain HTTP.
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export function createComposerDrafts() {
  const owners = new Map();
  const listeners = new Set();
  let state = new Map();
  const update = (owner, patch) => {
    state = new Map(state).set(owner, { ...state.get(owner), ...patch });
    for (const listener of listeners) listener();
  };
  return {
    get: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    /** Stable ownership through the start-page → created-session transition. */
    forKey(key) {
      if (!owners.has(key)) {
        const owner = { key };
        owners.set(key, owner);
        state = new Map(state).set(owner, { text: '', items: [], revision: 0, submission: null, retry: null });
      }
      return owners.get(key);
    },
    setText(owner, text) {
      const draft = state.get(owner);
      if (draft.text !== text) update(owner, { text, revision: draft.revision + 1 });
    },
    setItems(owner, change) {
      const items = state.get(owner).items;
      update(owner, { items: typeof change === 'function' ? change(items) : change });
    },
    begin(owner) {
      const draft = state.get(owner);
      if (draft.submission) return null;
      // A failed response may follow durable acceptance. Retry the same
      // content with its original ID so the server can return that receipt.
      const fingerprint = JSON.stringify([draft.text, draft.items]);
      const retry = draft.retry?.fingerprint === fingerprint ? draft.retry : { fingerprint, messageId: messageId() };
      const submission = {
        owner, text: draft.text, revision: draft.revision, items: draft.items,
        messageId: retry.messageId, sessionId: retry.sessionId ?? (owner.key || null),
      };
      update(owner, { submission, retry });
      return submission;
    },
    bindSession(submission, sessionId) {
      const draft = state.get(submission.owner);
      if (draft.submission === submission) update(submission.owner, { retry: { ...draft.retry, sessionId } });
    },
    move(submission, key) {
      const { owner } = submission;
      if (state.get(owner).submission !== submission || owners.has(key)) return false;
      owners.delete(owner.key);
      owner.key = key;
      owners.set(key, owner);
      return true;
    },
    finish(submission, accepted) {
      const { owner } = submission;
      const draft = state.get(owner);
      if (draft.submission !== submission) return false;
      const keys = new Set(submission.items.map((item) => item.key));
      update(owner, {
        submission: null,
        ...(accepted ? {
          retry: null,
          text: draft.revision === submission.revision ? '' : draft.text,
          items: draft.items.filter((item) => !keys.has(item.key)),
        } : {}),
      });
      return true;
    },
  };
}
