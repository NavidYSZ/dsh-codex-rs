#!/usr/bin/env python3
"""Tiny MCP test server (stdio or Streamable HTTP), no dependencies.

Usage:
  mcp_server.py                      stdio, newline-delimited JSON-RPC
  mcp_server.py --http PORTFILE      Streamable HTTP on 127.0.0.1:<random>, port written to PORTFILE
  mcp_server.py --http PORTFILE --sse   answer requests as SSE streams instead of JSON

Environment:
  MCP_TEST_INSTRUCTIONS   server instructions returned by initialize
  MCP_TEST_EVENTS         file that receives one JSON line per lifecycle event
  MCP_TEST_NO_RESOURCES   omit the resources capability
"""
import base64, json, os, subprocess, sys, threading, time

# 1x1 red PNG.
PNG = base64.b64encode(bytes.fromhex(
    '89504e470d0a1a0a0000000d4948445200000001000000010802000000907753de'
    '0000000c4944415408d763f8cfc000000301010018dd8db00000000049454e44ae426082')).decode()
SUPPORTED = ['2025-11-25', '2025-06-18', '2025-03-26', '2024-11-05']
EVENTS = os.environ.get('MCP_TEST_EVENTS')
cancelled = set()
extra_tools = []


def record(event, **extra):
    if EVENTS:
        with open(EVENTS, 'a') as f:
            f.write(json.dumps({'event': event, 'pid': os.getpid(), **extra}) + '\n')


def tools():
    base = [
        {'name': 'echo', 'description': 'Echo the given text back.', 'inputSchema': {
            'type': 'object', 'properties': {'text': {'type': 'string', 'description': 'Text to echo.'}},
            'required': ['text']}},
        {'name': 'image', 'description': 'Return a caption and a tiny PNG.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'bad_image', 'description': 'Return an image with a bad media type.', 'inputSchema': {'type': 'object'}},
        {'name': 'fail', 'description': 'Always fails.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'mixed', 'description': 'Return every content block kind.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'unknown_block', 'description': 'Return an unknown content block.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'empty', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'structured', 'description': 'Structured output.', 'inputSchema': {'type': 'object', 'properties': {}},
         'outputSchema': {'type': 'object', 'properties': {'n': {'type': 'number'}}, 'required': ['n']}},
        {'name': 'slow', 'description': 'Sleep for ms milliseconds.', 'inputSchema': {
            'type': 'object', 'properties': {'ms': {'type': 'integer'}}, 'required': ['ms']}},
        {'name': 'crash', 'description': 'Exit the server process.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'info', 'description': 'Report pid, cwd and selected environment variables.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'spawn_child', 'description': 'Start a long-running grandchild process.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'change', 'description': 'Add a tool and announce the list change.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'ping_client', 'description': 'Ping the client before answering.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'title': 'Ordered', 'name': 'ordered', 'annotations': {'readOnlyHint': True}, 'inputSchema': {
            '$schema': 'http://json-schema.org/draft-07/schema#', 'additionalProperties': False, 'required': ['a'],
            'properties': {'b': {'type': 'number', 'description': 'B'}, 'a': {'description': 'A', 'type': 'string'}},
            'type': 'object'}, 'description': 'Schema keys in unusual order.'},
        {'name': 'expire', 'description': 'HTTP: forget every session after answering.', 'inputSchema': {'type': 'object', 'properties': {}}},
        {'name': 'weird name.with/dots', 'description': 'A tool whose name needs normalizing.', 'inputSchema': {'type': 'object', 'properties': {}}},
    ]
    return base + extra_tools


def text(t):
    return {'type': 'text', 'text': t}


class Server:
    def __init__(self, send):
        self.send = send  # send(message) for server-initiated messages
        self.pending = {}
        self.next_id = 1000

    def request_client(self, method, params=None):
        ev = threading.Event()
        rid = self.next_id
        self.next_id += 1
        self.pending[rid] = [ev, None]
        self.send({'jsonrpc': '2.0', 'id': rid, 'method': method, **({'params': params} if params is not None else {})})
        ev.wait(10)
        return self.pending.pop(rid)[1]

    def handle(self, msg):
        """Returns a response dict, or None for notifications/responses."""
        if 'method' not in msg:
            slot = self.pending.get(msg.get('id'))
            if slot:
                slot[1] = msg
                slot[0].set()
            return None
        method, params, rid = msg['method'], msg.get('params') or {}, msg.get('id')
        if rid is None:
            if method == 'notifications/cancelled':
                cancelled.add(params.get('requestId'))
                record('cancelled', requestId=params.get('requestId'))
            elif method == 'notifications/initialized':
                record('initialized')
            return None
        record(method)
        try:
            result = self.dispatch(method, params, rid)
        except RpcError as e:
            return {'jsonrpc': '2.0', 'id': rid, 'error': {'code': e.code, 'message': e.message}}
        if result is None:
            return None
        return {'jsonrpc': '2.0', 'id': rid, 'result': result}

    def dispatch(self, method, params, rid):
        if method == 'initialize':
            version = params.get('protocolVersion')
            caps = {'tools': {'listChanged': True}}
            if not os.environ.get('MCP_TEST_NO_RESOURCES'):
                caps['resources'] = {}
            result = {'protocolVersion': version if version in SUPPORTED else SUPPORTED[0],
                      'capabilities': caps, 'serverInfo': {'name': 'fixture', 'version': '1.0.0'}}
            if os.environ.get('MCP_TEST_INSTRUCTIONS'):
                result['instructions'] = os.environ['MCP_TEST_INSTRUCTIONS']
            record('client', clientInfo=params.get('clientInfo'), protocolVersion=version)
            return result
        if method == 'ping':
            return {}
        if method == 'tools/list':
            all_tools = tools()
            if params.get('cursor') == 'page2':
                return {'tools': all_tools[8:]}
            return {'tools': all_tools[:8], 'nextCursor': 'page2'}
        if method == 'tools/call':
            return self.call(params.get('name'), params.get('arguments') or {}, rid)
        if method == 'resources/list':
            if params.get('cursor') == 'r2':
                return {'resources': [{'uri': 'test://blob', 'name': 'blob', 'mimeType': 'application/octet-stream'}]}
            return {'resources': [{'uri': 'test://readme', 'name': 'readme', 'mimeType': 'text/plain'}], 'nextCursor': 'r2'}
        if method == 'resources/templates/list':
            return {'resourceTemplates': [{'uriTemplate': 'test://item/{id}', 'name': 'item'}]}
        if method == 'resources/read':
            uri = params.get('uri')
            if uri == 'test://readme':
                return {'contents': [{'uri': uri, 'mimeType': 'text/plain', 'text': 'Hello from the readme.'}]}
            if uri == 'test://blob':
                return {'contents': [{'uri': uri, 'mimeType': 'application/octet-stream', 'blob': base64.b64encode(b'\x00\x01\x02binary').decode()}]}
            if uri.startswith('test://item/'):
                return {'contents': [{'uri': uri, 'text': 'item ' + uri.rsplit('/', 1)[1]}]}
            raise RpcError(-32002, f'Resource not found: {uri}')
        raise RpcError(-32601, f'Method not found: {method}')

    def call(self, name, args, rid):
        if name == 'echo':
            return {'content': [text(args.get('text', ''))]}
        if name == 'image':
            return {'content': [text('caption before'), {'type': 'image', 'data': PNG, 'mimeType': 'image/png'}, text('caption after')]}
        if name == 'bad_image':
            return {'content': [{'type': 'image', 'data': PNG, 'mimeType': 'image/bmp'}]}
        if name == 'fail':
            return {'content': [text('boom: the tool failed')], 'isError': True}
        if name == 'mixed':
            return {'content': [
                text('first'), text('second'),
                {'type': 'resource_link', 'uri': 'test://readme', 'name': 'readme'},
                {'type': 'audio', 'data': 'AAAA', 'mimeType': 'audio/wav'},
                {'type': 'resource', 'resource': {'uri': 'test://readme', 'text': 'inline'}},
            ]}
        if name == 'unknown_block':
            return {'content': [text('ok'), {'type': 'hologram'}]}
        if name == 'empty':
            return {'content': []}
        if name == 'structured':
            return {'content': [text('{"n": 7}')], 'structuredContent': {'n': 7}}
        if name == 'slow':
            deadline = time.time() + args.get('ms', 0) / 1000
            while time.time() < deadline:
                if rid in cancelled:
                    record('slow-cancelled')
                    return None
                time.sleep(0.01)
            return {'content': [text('slept')]}
        if name == 'crash':
            record('crash')
            os._exit(3)
        if name == 'info':
            env = {k: os.environ.get(k) for k in ('MCP_TEST_VAR', 'FAKE_API_KEY', 'PATH_PROBE')}
            return {'content': [text(json.dumps({'pid': os.getpid(), 'cwd': os.getcwd(), 'env': env}))]}
        if name == 'spawn_child':
            child = subprocess.Popen(['sleep', '1000'])
            return {'content': [text(str(child.pid))]}
        if name == 'change':
            if not any(t['name'] == 'added' for t in extra_tools):
                extra_tools.append({'name': 'added', 'description': 'Added at runtime.', 'inputSchema': {'type': 'object', 'properties': {}}})
            self.send({'jsonrpc': '2.0', 'method': 'notifications/tools/list_changed'})
            return {'content': [text('changed')]}
        if name == 'added':
            return {'content': [text('added works')]}
        if name == 'ping_client':
            reply = self.request_client('ping')
            return {'content': [text('client answered: ' + json.dumps(reply.get('result') if reply else None))]}
        if name == 'weird name.with/dots':
            return {'content': [text('weird ok')]}
        if name == 'expire':
            return {'content': [text('expired')]}
        if name == 'ordered':
            return {'content': [text('ordered ' + json.dumps(args, sort_keys=True))]}
        raise RpcError(-32602, f'Unknown tool: {name}')


class RpcError(Exception):
    def __init__(self, code, message):
        self.code, self.message = code, message


def serve_stdio():
    lock = threading.Lock()

    def send(msg):
        with lock:
            sys.stdout.write(json.dumps(msg) + '\n')
            sys.stdout.flush()

    server = Server(send)
    record('start')

    def work(msg):
        reply = server.handle(msg)
        if reply is not None:
            send(reply)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        # Requests run on threads so slow calls, cancellation and client pings interleave.
        threading.Thread(target=work, args=(msg,), daemon=True).start()
    record('stdin-closed')


def serve_http(portfile, sse):
    from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
    sessions = {}

    class H(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *a):
            pass

        def reply(self, status, body=b'', ctype='application/json', headers=None):
            self.send_response(status)
            self.send_header('content-type', ctype)
            self.send_header('content-length', str(len(body)))
            for k, v in (headers or {}).items():
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            self.reply(405, b'')

        def do_DELETE(self):
            sid = self.headers.get('mcp-session-id')
            record('delete', session=sid, auth=self.headers.get('authorization'))
            sessions.pop(sid, None)
            self.reply(200, b'')

        def do_POST(self):
            n = int(self.headers.get('content-length', 0))
            msg = json.loads(self.rfile.read(n))
            sid = self.headers.get('mcp-session-id')
            record('post', method=msg.get('method'), session=sid, auth=self.headers.get('authorization'),
                   accept=self.headers.get('accept'), version=self.headers.get('mcp-protocol-version'))
            if msg.get('method') == 'server/discover':
                self.reply(200, json.dumps({'jsonrpc': '2.0', 'id': msg.get('id'), 'error': {'code': -32601, 'message': 'Method not found: server/discover'}}).encode())
                return
            if msg.get('method') != 'initialize' and sid not in sessions:
                self.reply(404 if sid else 400, b'{"error":"unknown session"}')
                return
            outbox = []
            server = sessions.get(sid) or Server(outbox.append)
            server.send = outbox.append
            reply = server.handle(msg)
            headers = {}
            if msg.get('method') == 'initialize':
                sid = 'sess-' + str(len(sessions) + 1)
                sessions[sid] = server
                headers['mcp-session-id'] = sid
            if msg.get('method') == 'tools/call' and (msg.get('params') or {}).get('name') == 'expire':
                # Forget every session, as a restarted server would.
                sessions.clear()
            if reply is None:
                self.reply(202, b'', headers=headers)
                return
            if sse:
                frames = ''.join(f'event: message\ndata: {json.dumps(m)}\n\n' for m in outbox + [reply])
                self.reply(200, (': keepalive\n\n' + frames).encode(), 'text/event-stream', headers)
            else:
                self.reply(200, json.dumps(reply).encode(), headers=headers)

    httpd = ThreadingHTTPServer(('127.0.0.1', 0), H)
    with open(portfile, 'w') as f:
        f.write(str(httpd.server_address[1]))
    record('start')
    httpd.serve_forever()


if __name__ == '__main__':
    if len(sys.argv) > 2 and sys.argv[1] == '--http':
        serve_http(sys.argv[2], '--sse' in sys.argv)
    else:
        serve_stdio()
