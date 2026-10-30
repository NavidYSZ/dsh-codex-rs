// node --test ui/dev/composer_check.mjs
// Real UI modules, with only browser globals and server I/O replaced.
import assert from 'node:assert/strict';
import { registerHooks } from 'node:module';
import { test } from 'node:test';
import { createComposerDrafts } from '../src/composer-drafts.js';

const vendor = new URL('../vendor/', import.meta.url);
const map = { preact: 'preact.mjs', 'preact/hooks': 'hooks.mjs', htm: 'htm.mjs', marked: 'marked.mjs' };
registerHooks({
  resolve: (spec, ctx, next) => (map[spec] ? { url: new URL(map[spec], vendor).href, shortCircuit: true } : next(spec, ctx)),
});
globalThis.location = { search: '?lang=en', hash: '', pathname: '/', protocol: 'http:', host: 'localhost' };
globalThis.history = {
  pushState(_state, _title, url) { location.hash = new URL(url, 'http://localhost').hash; },
  replaceState(_state, _title, url) { location.hash = new URL(url, 'http://localhost').hash; },
};
globalThis.window = { addEventListener() {}, matchMedia: () => ({ matches: false, addEventListener() {} }) };
globalThis.document = { documentElement: { dataset: {} }, visibilityState: 'hidden' };
globalThis.localStorage = { getItem: () => null, setItem() {} };

const { pendingPrompts, queuedPrompts, steerQueued } = await import('../src/components/composer-extras.js');
const { store, api, send, newChat } = await import('../src/state.js');
const { modeText } = await import('../src/components/modes.js');
api.sessions = async () => [];
api.updateSession = async () => null;
api.stream = () => () => {};
api.session = async (id) => ({ header: { id, agentPreset: 'ptc' }, events: [], status: 'idle' });

const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const message = (id, text = id, source = { kind: 'user' }) => ({ id, source, content: [{ type: 'text', text }] });
const splice = (target, start, removedCount, inserted = []) => ({ type: 'agent/inbox/spliced', data: { target, start, removedCount, inserted } });

test('both inboxes are visible and a steered queue item appears exactly once', () => {
  const events = [
    splice('next-turn', 0, 0, [message('queued'), message('internal', '', { kind: 'schedule' })]),
    splice('next-step', 0, 0, [message('steered')]),
  ];
  assert.deepEqual(pendingPrompts(events).map((m) => [m.id, m.target]), [['steered', 'next-step'], ['queued', 'next-turn']]);
  assert.deepEqual(queuedPrompts(events).map((m) => m.id), ['queued']);
  events.push(splice('next-turn', 0, 1), splice('next-step', 1, 0, [message('queued')]));
  assert.deepEqual(pendingPrompts(events).map((m) => m.id), ['steered', 'queued']);
  assert.deepEqual(queuedPrompts(events), []);
  events.push(splice('next-step', 0, 2));
  assert.deepEqual(pendingPrompts(events), []);
});

test('queue edits and removals fold correctly, including legacy user sources', () => {
  const events = [splice('next-turn', 0, 0, [message('a', 'before', 'user'), message('b')])];
  events.push(splice('next-turn', 0, 1, [message('a', 'after', 'user')]));
  assert.equal(pendingPrompts(events)[0].content[0].text, 'after');
  events.push(splice('next-turn', 1, 1));
  assert.deepEqual(pendingPrompts(events).map((m) => m.id), ['a']);
});

test('draft and attachments remain intact until acceptance; duplicate submit is rejected', () => {
  const drafts = createComposerDrafts();
  const owner = drafts.forKey('a');
  const items = [{ key: 'image', ref: { attachmentId: 'image' } }, { key: 'file', saved: { path: '/file.txt' } }];
  drafts.setText(owner, '  exact draft\n');
  drafts.setItems(owner, items);
  const submission = drafts.begin(owner);
  assert.equal(drafts.begin(owner), null);
  assert.equal(drafts.get().get(owner).text, '  exact draft\n');
  assert.deepEqual(drafts.get().get(owner).items, items);
  drafts.finish(submission, false);
  assert.equal(drafts.get().get(owner).text, '  exact draft\n');
  assert.deepEqual(drafts.get().get(owner).items, items);
  drafts.finish(drafts.begin(owner), true);
  assert.equal(drafts.get().get(owner).text, '');
  assert.deepEqual(drafts.get().get(owner).items, []);
});

test('acceptance preserves later text and attachments, even text edited back to its original value', () => {
  const drafts = createComposerDrafts();
  const owner = drafts.forKey('a');
  drafts.setText(owner, 'original');
  drafts.setItems(owner, [{ key: 'old' }]);
  const submission = drafts.begin(owner);
  drafts.setText(owner, 'new text');
  drafts.setText(owner, 'original');
  drafts.setItems(owner, (items) => [...items, { key: 'new' }]);
  drafts.finish(submission, true);
  assert.equal(drafts.get().get(owner).text, 'original');
  assert.deepEqual(drafts.get().get(owner).items, [{ key: 'new' }]);
  const next = drafts.begin(owner);
  assert.equal(drafts.finish(submission, false), false);
  assert.equal(drafts.get().get(owner).submission, next);
});

test('uncertain failures reuse a UUID until the submitted text or attachments change', () => {
  const drafts = createComposerDrafts();
  const owner = drafts.forKey('retry-chat');
  drafts.setText(owner, 'original');
  drafts.setItems(owner, [{ key: 'image', ref: { attachmentId: 'original-image' } }]);
  const first = drafts.begin(owner);
  assert.match(first.messageId, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  drafts.finish(first, false);
  const retry = drafts.begin(owner);
  assert.equal(retry.messageId, first.messageId);
  drafts.finish(retry, false);
  drafts.setText(owner, 'changed text');
  const changedText = drafts.begin(owner);
  assert.notEqual(changedText.messageId, first.messageId);
  drafts.finish(changedText, false);
  drafts.setItems(owner, [{ key: 'image', ref: { attachmentId: 'changed-image' } }]);
  const changedImage = drafts.begin(owner);
  assert.notEqual(changedImage.messageId, changedText.messageId);
  drafts.finish(changedImage, false);
  drafts.setItems(owner, (items) => [...items, { key: 'file', saved: { path: '/new.txt' } }]);
  assert.notEqual(drafts.begin(owner).messageId, changedImage.messageId);
});

test('a later intentional send gets a new UUID after confirmed acceptance', () => {
  const drafts = createComposerDrafts();
  const owner = drafts.forKey('repeat-chat');
  drafts.setText(owner, 'again');
  const first = drafts.begin(owner);
  drafts.finish(first, true);
  drafts.setText(owner, 'again');
  assert.notEqual(drafts.begin(owner).messageId, first.messageId);
});

test('late completion belongs to its original chat, including attachments', () => {
  const drafts = createComposerDrafts();
  const first = drafts.forKey('a');
  drafts.setText(first, 'first chat');
  const submission = drafts.begin(first);
  const second = drafts.forKey('b');
  drafts.setText(second, 'second chat');
  drafts.setItems(second, [{ key: 'second-image' }]);
  drafts.finish(submission, true);
  assert.equal(drafts.get().get(first).text, '');
  assert.equal(drafts.get().get(second).text, 'second chat');
  assert.deepEqual(drafts.get().get(second).items, [{ key: 'second-image' }]);
});

test('a newly created session inherits draft ownership and late uploads stay with it', () => {
  const drafts = createComposerDrafts();
  const owner = drafts.forKey('');
  drafts.setText(owner, 'first prompt');
  drafts.setItems(owner, [{ key: 'submitted-image' }]);
  const submission = drafts.begin(owner);
  drafts.setItems(owner, (items) => [...items, { key: 'uploading-image' }]);
  assert.equal(drafts.move(submission, 'created'), true);
  assert.equal(drafts.forKey('created'), owner);
  const fresh = drafts.forKey('');
  drafts.setText(fresh, 'another new chat');
  drafts.setItems(owner, (items) => items.map((item) => item.key === 'uploading-image' ? { ...item, ref: 'complete' } : item));
  drafts.finish(submission, true);
  assert.equal(drafts.get().get(owner).text, '');
  assert.deepEqual(drafts.get().get(owner).items, [{ key: 'uploading-image', ref: 'complete' }]);
  assert.equal(drafts.get().get(fresh).text, 'another new chat');
});

test('new-session send keeps its transferred draft until the server accepts', async () => {
  newChat();
  store.set({ composer: { preset: 'ptc' } });
  const drafts = createComposerDrafts();
  const owner = drafts.forKey('');
  drafts.setText(owner, 'start in PTC');
  drafts.setItems(owner, [{ key: 'image' }]);
  const submission = drafts.begin(owner);
  const accepted = deferred();
  const called = deferred();
  api.createSession = async (_cwd, preset) => { assert.equal(preset, 'ptc'); return { id: 'created-for-send' }; };
  api.prompt = async (id) => { called.resolve(id); await accepted.promise; };
  const sending = send(submission.text, [], {}, {
    onSession(id, following) { assert.equal(following, true); drafts.move(submission, id); },
  });
  assert.equal(await called.promise, 'created-for-send');
  assert.equal(store.get().activeId, 'created-for-send');
  assert.equal(drafts.forKey(store.get().activeId), owner);
  assert.equal(drafts.get().get(owner).text, 'start in PTC');
  assert.deepEqual(drafts.get().get(owner).items, [{ key: 'image' }]);
  accepted.resolve();
  drafts.finish(submission, await sending);
  assert.equal(drafts.get().get(owner).text, '');
});

test('session creation does not reopen a chat after the user navigates away', async () => {
  newChat();
  const created = deferred();
  let following;
  let prompted;
  api.createSession = () => created.promise;
  api.prompt = async (id) => { prompted = id; };
  const sending = send('first', [], {}, { onSession(_id, value) { following = value; } });
  newChat(); // Even another start page counts as navigation.
  created.resolve({ id: 'background-created' });
  assert.equal(await sending, true);
  assert.equal(following, false);
  assert.equal(prompted, 'background-created');
  assert.equal(store.get().activeId, null);
});

test('a rejected request does not clear another chat pending message', async () => {
  store.set({ activeId: 'first', status: 'idle', pending: null });
  const response = deferred();
  api.prompt = () => response.promise;
  const sending = send('first', [], {});
  const other = { text: 'second', images: [] };
  store.set({ activeId: 'second', pending: other });
  response.reject(Object.assign(new Error('offline'), { status: 0 }));
  assert.equal(await sending, false);
  assert.equal(store.get().pending, other);
});

test('a delayed acknowledgement does not resurrect a turn that already completed', async () => {
  store.set({ activeId: 'quick-turn', status: 'idle', pending: null });
  const response = deferred();
  api.prompt = () => response.promise;
  const sending = send('finish quickly', [], {});
  store.set({ status: 'running' });
  store.set({ status: 'idle', pending: null });
  response.resolve();
  assert.equal(await sending, true);
  assert.equal(store.get().status, 'idle');
});

test('send forwards the same messageId on a failed retry, including after session creation', async () => {
  newChat();
  const drafts = createComposerDrafts();
  const owner = drafts.forKey('');
  drafts.setText(owner, 'retry this message');
  api.createSession = async () => ({ id: 'retry-created' });
  const received = [];
  api.prompt = async (id, body) => {
    received.push({ id, body });
    if (received.length === 1) throw Object.assign(new Error('offline'), { status: 0 });
  };
  const first = drafts.begin(owner);
  drafts.finish(first, await send(first.text, [], { messageId: first.messageId }, {
    onSession(id, following) { assert.equal(following, true); drafts.bindSession(first, id); drafts.move(first, id); },
  }));
  assert.equal(store.get().activeId, 'retry-created');
  assert.equal(drafts.forKey('retry-created'), owner);
  const retry = drafts.begin(owner);
  drafts.finish(retry, await send(retry.text, [], { messageId: retry.messageId }));
  assert.equal(received.length, 2);
  assert.deepEqual(received[0], received[1]);
  assert.equal(received[1].body.messageId, first.messageId);
  assert.equal(drafts.get().get(owner).text, '');
});

test('retry preserves a background-created destination after navigation and a lost acknowledgement', async () => {
  newChat();
  const drafts = createComposerDrafts();
  const owner = drafts.forKey('');
  drafts.setText(owner, 'same message in the original chat');
  const first = drafts.begin(owner);
  const created = deferred();
  const received = [];
  let creations = 0;
  api.createSession = () => { creations++; return created.promise; };
  api.prompt = async (id, body) => {
    received.push({ id, messageId: body.messageId });
    if (received.length === 1) throw Object.assign(new Error('lost response'), { status: 0 });
  };
  const lifecycle = (submission) => ({
    sessionId: submission.sessionId,
    onSession(id, following) {
      drafts.bindSession(submission, id);
      if (following) drafts.move(submission, id);
    },
  });
  const sending = send(first.text, [], { messageId: first.messageId }, lifecycle(first));
  newChat();
  created.resolve({ id: 'background-destination' });
  drafts.finish(first, await sending);
  assert.equal(store.get().activeId, null);
  assert.equal(drafts.forKey(''), owner);
  const retry = drafts.begin(owner);
  assert.equal(retry.sessionId, 'background-destination');
  assert.equal(retry.messageId, first.messageId);
  const originalSession = api.session;
  api.session = async (id) => ({ header: { id }, status: 'idle', events: [
    { type: 'user/message', seq: 0, data: { id: first.messageId, source: { kind: 'user' } } },
  ] });
  try {
    drafts.finish(retry, await send(retry.text, [], { messageId: retry.messageId }, lifecycle(retry)));
    assert.equal(creations, 1);
    assert.deepEqual(received[0], received[1]);
    assert.equal(store.get().activeId, 'background-destination');
    assert.equal(store.get().pending, null);
    assert.equal(drafts.get().get(owner).text, '');
  } finally { api.session = originalSession; }
});

test('steer-all keeps targeting its original chat after navigation', async () => {
  store.set({ activeId: 'queued-chat', status: 'running', events: [splice('next-turn', 0, 0, [message('first'), message('second')])] });
  const response = deferred();
  const calls = [];
  api.updateQueue = async (id, item) => { calls.push([id, item]); if (calls.length === 1) await response.promise; };
  const steering = steerQueued();
  store.set({ activeId: 'other-chat' });
  response.resolve();
  assert.equal(await steering, true);
  assert.deepEqual(calls, [['queued-chat', 'first'], ['queued-chat', 'second']]);
});

test('PTC is localized and unknown server-provided modes remain supported', () => {
  assert.equal(modeText('ptc').name, 'PTC');
  assert.match(modeText('ptc').description, /TypeScript/);
  assert.deepEqual(modeText('server-mode', [{ id: 'server-mode', name: 'From server', description: 'Server behavior' }]), {
    name: 'From server', hint: 'Server behavior', description: 'Server behavior',
  });
});
