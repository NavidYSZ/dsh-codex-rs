// A fresh Node process per run_code. This is an evaluator, not a security
// boundary: the parent applies the OS file sandbox and owns the deadline.
import { Socket } from 'node:net';
import { inspect } from 'node:util';

const stringify = JSON.stringify;
const parse = JSON.parse;
const ownKeys = Reflect.ownKeys;
const descriptors = Object.getOwnPropertyDescriptors;
const prototypeOf = Object.getPrototypeOf;
const objectPrototype = Object.prototype;
const create = Object.create;
const define = Object.defineProperty;
const isArray = Array.isArray;
const NativeError = Error;
const NativePromise = Promise;
const byteLength = Buffer.byteLength;
const exit = process.exit.bind(process);
const MAX_FRAME = 8 * 1024 * 1024;
const MAX_OUTPUT = 4 * 1024 * 1024;
const MAX_PENDING = 128;
const MAX_CALLS = 4096;
const MAX_LOGS = 4096;

// No credentials or native startup options are inherited. Replacing env also
// prevents accidental exposure through process.env in the program.
for (const key of Object.keys(process.env)) delete process.env[key];
process.env = create(null);
const socket = new Socket({ fd: 3, readable: true, writable: true });
socket.on('error', () => exit(1));
socket.on('close', () => exit(0));

let finished = false;
let outputBytes = 2;
let logCount = 0;
let nextId = 1;
const pending = new Map();

function send(value) {
  const body = Buffer.from(stringify(value));
  if (body.length > MAX_FRAME || socket.writableLength + body.length + 4 > MAX_FRAME) {
    throw new NativeError('control channel exceeds its byte limit');
  }
  const frame = Buffer.allocUnsafe(body.length + 4);
  frame.writeUInt32BE(body.length);
  body.copy(frame, 4);
  socket.write(frame);
}

function finish(error, value) {
  if (finished) return;
  finished = true;
  const message = { type: 'done' };
  if (error !== undefined) message.error = error;
  else if (value !== undefined) message.value = value;
  try { send(message); } catch { exit(1); }
}

function outputLimit() {
  finish({ kind: 'output-limit', message: `outer output exceeded ${MAX_OUTPUT} bytes or ${MAX_LOGS} entries` });
}

function log(text) {
  if (finished) return;
  const size = byteLength(stringify(text));
  if (++logCount > MAX_LOGS || outputBytes + size + 1 > MAX_OUTPUT) {
    outputLimit();
    return;
  }
  outputBytes += size + 1;
  try { send({ type: 'log', text }); }
  catch { finish({ kind: 'protocol', message: 'control channel exceeds its byte limit' }); }
}

const consoleShim = create(null);
for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
  consoleShim[level] = (...args) => log(args.map(value => typeof value === 'string'
    ? value : inspect(value, { depth: 4, maxArrayLength: 100, maxStringLength: 10000 })).join(' '));
}
globalThis.console = consoleShim;
for (const stream of [process.stdout, process.stderr]) {
  stream.write = (chunk, encoding, callback) => {
    log(typeof chunk === 'string' ? chunk : Buffer.from(chunk).toString(typeof encoding === 'string' ? encoding : 'utf8'));
    const cb = typeof encoding === 'function' ? encoding : callback;
    if (typeof cb === 'function') queueMicrotask(() => cb(null));
    return true;
  };
}

// Reject values JSON would silently change (undefined members, sparse arrays,
// dates, getters, cycles, NaN, etc.). Detached records have no prototype setters.
function snapshot(value) {
  const ancestors = new Set();
  let nodes = 0;
  let bytes = 0;
  function copy(item, depth) {
    if (++nodes > 100000 || depth > 64) throw new NativeError('JSON value is too complex');
    if (item === null || typeof item === 'boolean') return item;
    if (typeof item === 'number' && Number.isFinite(item) && !Object.is(item, -0)) return item;
    if (typeof item === 'string') {
      bytes += byteLength(item);
      if (bytes > MAX_FRAME) throw new NativeError('JSON value is too large');
      return item;
    }
    if (typeof item !== 'object' || ancestors.has(item)) throw new NativeError('value must be lossless JSON');
    const array = isArray(item);
    if (!array && prototypeOf(item) !== null && prototypeOf(item) !== objectPrototype) {
      throw new NativeError('value must be a JSON record or array');
    }
    ancestors.add(item);
    const props = descriptors(item);
    const result = array ? [] : create(null);
    let count = 0;
    for (const key of ownKeys(props)) {
      if (array && key === 'length') continue;
      const prop = props[key];
      if (typeof key !== 'string' || !prop.enumerable || !Object.hasOwn(prop, 'value')) {
        throw new NativeError('value must contain only enumerable JSON data');
      }
      if (array && (String(count) !== key || count >= item.length)) throw new NativeError('array must be dense');
      bytes += byteLength(key);
      if (bytes > MAX_FRAME) throw new NativeError('JSON value is too large');
      define(result, key, { value: copy(prop.value, depth + 1), enumerable: true, writable: true, configurable: true });
      count++;
    }
    if (array && count !== item.length) throw new NativeError('array must be dense');
    ancestors.delete(item);
    return result;
  }
  return copy(value, 0);
}

class ToolCallError extends NativeError {
  constructor(toolName, message) {
    super(message);
    define(this, 'name', { value: 'ToolCallError', enumerable: true });
    define(this, 'toolName', { value: toolName, enumerable: true });
  }
}

function makeTools(names) {
  const tools = create(null);
  for (const name of names) {
    define(tools, name, { enumerable: true, value: args => {
      if (finished) return NativePromise.reject(new ToolCallError(name, 'run_code run is over'));
      let detached;
      try {
        detached = snapshot(args);
        if (pending.size >= MAX_PENDING || nextId > MAX_CALLS) throw new NativeError('binding call limit exceeded');
      } catch (error) {
        return NativePromise.reject(new ToolCallError(name, `binding arguments must be lossless JSON: ${error.message}`));
      }
      return new NativePromise((resolve, reject) => {
        const id = nextId;
        pending.set(id, { resolve, reject, name });
        try {
          send({ type: 'call', id, name, args: detached });
          nextId++;
        } catch (error) {
          pending.delete(id);
          reject(new ToolCallError(name, error.message));
        }
      });
    } });
  }
  return tools;
}

async function run(boot) {
  try {
    const [major, minor] = process.versions.node.split('.').map(Number);
    if (major < 22 || (major === 22 && minor < 18)) {
      throw new NativeError('run_code requires Node.js 22.18 or newer; configure DSHX_NODE');
    }
    const { stripTypeScriptTypes } = await import('node:module');
    // Parse as an async function so both await and return are valid. Like the
    // upstream runtime, support erasable TS only (no enums or parameter properties).
    const code = stripTypeScriptTypes(`async function __dshx_program__() {\n${boot.code}\n}`, {
      mode: 'strip', sourceUrl: 'run_code.ts'
    });
    const execute = new Function('tools', 'ToolCallError', 'console', `"use strict";\n${code}\nreturn __dshx_program__();`);
    const value = await execute(makeTools(boot.names), ToolCallError, consoleShim);
    if (value === undefined) { finish(); return; }
    let detached;
    try { detached = snapshot(value); }
    catch { finish({ kind: 'invalid-output', message: 'program completion must be lossless JSON' }); return; }
    if (outputBytes + byteLength(stringify(detached)) > MAX_OUTPUT) { outputLimit(); return; }
    finish(undefined, detached);
  } catch (error) {
    let message;
    try { message = error instanceof NativeError ? error.stack ?? error.message : String(error); }
    catch { message = 'program threw an unrenderable value'; }
    if (outputBytes + byteLength(stringify(message)) > MAX_OUTPUT) outputLimit();
    else finish({ kind: 'exception', message });
  }
}

// A socket chunk can end one maximum-size frame and begin another. Charge
// only the declared frame, never the aggregate bytes of a coalesced read.
const header = Buffer.alloc(4);
let headerBytes = 0;
let body;
let bodyBytes = 0;
let booted = false;
socket.on('data', chunk => {
  let offset = 0;
  while (offset < chunk.length) {
    if (headerBytes < 4) {
      const count = Math.min(4 - headerBytes, chunk.length - offset);
      chunk.copy(header, headerBytes, offset, offset + count);
      headerBytes += count;
      offset += count;
      if (headerBytes < 4) continue;
      const size = header.readUInt32BE(0);
      if (!size || size > MAX_FRAME) { exit(1); return; }
      body = Buffer.allocUnsafe(size);
      bodyBytes = 0;
    }
    const count = Math.min(body.length - bodyBytes, chunk.length - offset);
    chunk.copy(body, bodyBytes, offset, offset + count);
    bodyBytes += count;
    offset += count;
    if (bodyBytes < body.length) continue;
    let message;
    try { message = parse(body.toString('utf8')); }
    catch { exit(1); return; }
    headerBytes = 0;
    body = undefined;
    if (!booted) {
      if (message.type !== 'boot') { exit(1); return; }
      booted = true;
      void run(message);
    } else {
      const entry = pending.get(message.id);
      if (!entry) continue;
      pending.delete(message.id);
      if (message.ok === true) entry.resolve(message.value);
      else entry.reject(new ToolCallError(entry.name, message.message));
    }
  }
});
// Unawaited binding errors and timers must still produce a failed run.
process.on('unhandledRejection', error => finish({ kind: 'exception', message: String(error) }));
process.on('uncaughtException', error => finish({ kind: 'exception', message: String(error) }));
send({ type: 'ready' });
