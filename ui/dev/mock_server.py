"""A stand-in for dshx-server that speaks SPEC §6 over real HTTP + WebSocket.

    python3 ui/dev/mock_server.py [PORT]      # prints http://127.0.0.1:PORT/?token=…

Standard library only. Serves ui/ as static files and the /api routes from the
fixtures: every fixture session is listed, its live tail is replayed over the
WebSocket (events, deltas, status, approval) when a client subscribes, approvals
play the fixture's onApproval events, prompts get a canned reply, uploads are
kept in memory. It exists to exercise api.js against the real protocol; it is
not a model of how the agent behaves.
"""
import base64, hashlib, json, mimetypes, pathlib, queue, secrets, select, socket, struct, sys, threading, time, uuid
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs, unquote

UI = pathlib.Path(__file__).resolve().parent.parent
FIX = UI / 'fixtures'
TOKEN = secrets.token_urlsafe(18)
mimetypes.add_type('text/javascript', '.mjs')
mimetypes.add_type('text/javascript', '.js')

INFO = {
    'version': '0.3.0-mock', 'platform': sys.platform, 'locale': 'en',
    'models': [
        {'id': 'deepseek-flash', 'name': 'deepseek-flash', 'contextWindow': 1000000, 'inputModalities': ['text', 'image'],
         'reasoningEfforts': ['off', 'low', 'high', 'max'], 'defaultReasoningEffort': 'high'},
        {'id': 'deepseek-v4-pro', 'name': 'deepseek-v4-pro', 'contextWindow': 1000000, 'inputModalities': ['text'],
         'reasoningEfforts': ['off', 'low', 'high', 'max'], 'defaultReasoningEffort': 'high'},
    ],
    'settings': {'model': 'deepseek-flash', 'effort': 'high', 'sandbox': 'workspace-write', 'approval': 'ask'},
    'apiKeyConfigured': False,
    'tools': {'browser': True, 'app': sys.platform == 'darwin', 'computer': sys.platform == 'darwin'},
}


class Session:
    def __init__(self, header, events, live=None, approval=None, on_approval=None, title='', status='idle'):
        self.header = header
        self.events = list(events if live is None else events[:live])
        self.pending = [] if live is None else list(events[live:])
        self.approval = approval
        self.on_approval = on_approval or {}
        self.title = title
        self.status = status
        self.subs = set()
        self.lock = threading.Lock()
        self.updated = int(time.time() * 1000)

    def broadcast(self, msg):
        for q in list(self.subs):
            q.put(msg)

    def append(self, event):
        with self.lock:
            ev = dict(event)
            ev['seq'] = (self.events[-1]['seq'] + 1) if self.events else 0
            ev['time'] = int(time.time() * 1000)
            self.events.append(ev)
            self.updated = ev['time']
        if ev['type'] == 'session/title':
            self.title = ev['data'].get('title', self.title)
        self.broadcast({'kind': 'event', 'sessionId': self.header['id'], 'event': ev})

    def set_status(self, status):
        self.status = status
        self.broadcast({'kind': 'status', 'sessionId': self.header['id'], 'status': status})

    def play(self, events):
        """Streams like dshx-server would: deltas first, then the settling event."""
        for raw in events:
            d = raw.get('data', {})
            if raw['type'] == 'assistant/message':
                for index, block in enumerate(d['message']['content']):
                    if block['type'] not in ('text', 'reasoning'):
                        continue
                    text = block['text']
                    for i in range(0, len(text), 12):
                        self.broadcast({'kind': 'delta', 'sessionId': self.header['id'], 'turn': d['turn'], 'step': d['step'],
                                        'index': index, 'blockType': block['type'], 'text': text[i:i + 12]})
                        time.sleep(0.012)
            elif raw['type'] == 'tool/result':
                time.sleep(0.9 if (d.get('meta') or {}).get('tool') in ('browser', 'computer', 'app') else 0.25)
            elif raw['type'] == 'llm/retry-started':
                time.sleep(1.2)
            else:
                time.sleep(0.02)
            self.append({k: v for k, v in raw.items() if k not in ('seq', 'time')})

    def replay_tail(self):
        if not self.pending and not self.approval:
            return
        tail, self.pending = self.pending, []
        self.set_status('running')
        self.play(tail)
        if self.approval:
            self.set_status('waiting-approval')
            self.broadcast({'kind': 'approval', 'sessionId': self.header['id'], **self.approval})
        else:
            self.set_status('idle')


SESSIONS = {}
WORKSPACES = []
ATTACHMENTS = {}


def load():
    shared = json.loads((FIX / '_shared.json').read_text())
    now = int(time.time() * 1000)
    for w in shared['workspaces']:
        WORKSPACES.append({'path': w['path'], 'name': w['name'], 'lastUsed': now - w.get('lastUsedAgo', 0)})
    for entry in shared['sessions']:
        fx = json.loads((FIX / f"{entry['fixture']}.json").read_text())
        s = fx['session']
        sess = Session(s['header'], s['events'], fx.get('live'), fx.get('approval'), fx.get('onApproval'), entry['title'])
        sess.updated = now - entry.get('updatedAgo', 0)
        SESSIONS[s['header']['id']] = sess
        for att, path in (fx.get('attachments') or {}).items():
            ATTACHMENTS[att] = ((FIX / path).read_bytes(), 'image/jpeg')


def ws_frame(text):
    data = text.encode()
    n = len(data)
    if n < 126:
        head = struct.pack('!BB', 0x81, n)
    elif n < 65536:
        head = struct.pack('!BBH', 0x81, 126, n)
    else:
        head = struct.pack('!BBQ', 0x81, 127, n)
    return head + data


class Handler(SimpleHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def __init__(self, *a, **kw):
        super().__init__(*a, directory=str(UI), **kw)

    def log_message(self, *a):
        pass

    # ---- helpers
    def send_json(self, code, value):
        body = json.dumps(value).encode()
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def send_empty(self, code):
        self.send_response(code)
        self.send_header('Content-Length', '0')
        self.end_headers()

    def body(self):
        n = int(self.headers.get('Content-Length') or 0)
        return self.rfile.read(n) if n else b''

    def authed(self, query):
        return self.headers.get('Authorization') == f'Bearer {TOKEN}' or query.get('token', [''])[0] == TOKEN

    def info(self):
        return {**INFO, 'settings': dict(INFO['settings'])}

    def session_row(self, s):
        return {'id': s.header['id'], 'title': s.title, 'cwd': s.header['cwd'], 'createdAt': s.header['createdAt'],
                'updatedAt': s.updated, 'status': s.status}

    # ---- routing
    def route(self, method):
        url = urlparse(self.path)
        query = parse_qs(url.query)
        parts = [unquote(p) for p in url.path.split('/')[1:]]
        if parts[0] != 'api':
            if method == 'GET':
                return super().do_GET()
            return self.send_empty(405)
        if not self.authed(query if parts[1:2] == ['stream'] else {}):
            return self.send_json(401, {'error': 'missing or wrong token'})
        p = parts[1:]
        if method == 'GET' and p == ['info']:
            return self.send_json(200, self.info())
        if method == 'PUT' and p == ['settings']:
            patch = json.loads(self.body() or b'{}')
            if patch.pop('apiKey', None):
                INFO['apiKeyConfigured'] = True
            INFO['settings'].update({k: v for k, v in patch.items() if v is not None})
            return self.send_json(200, self.info())
        if p == ['workspaces']:
            if method == 'GET':
                return self.send_json(200, WORKSPACES)
            path = json.loads(self.body() or b'{}').get('path', '')
            if not path.startswith('/'):
                return self.send_json(400, {'error': 'path must be absolute'})
            ws = {'path': path, 'name': path.rstrip('/').rsplit('/', 1)[-1], 'lastUsed': int(time.time() * 1000)}
            WORKSPACES.insert(0, ws)
            return self.send_json(200, ws)
        if p == ['sessions']:
            if method == 'GET':
                cwd = query.get('cwd', [None])[0]
                rows = [self.session_row(s) for s in SESSIONS.values() if cwd is None or s.header['cwd'] == cwd]
                return self.send_json(200, sorted(rows, key=lambda r: -r['updatedAt']))
            cwd = json.loads(self.body() or b'{}').get('cwd', '')
            sid = f'session-{uuid.uuid4()}'
            SESSIONS[sid] = Session({'type': 'session', 'version': 4, 'id': sid, 'createdAt': int(time.time() * 1000), 'cwd': cwd,
                                     'isSeeded': False, 'delegationDepth': 0}, [])
            return self.send_json(200, {'id': sid})
        if len(p) >= 2 and p[0] == 'sessions':
            s = SESSIONS.get(p[1])
            if not s:
                return self.send_json(404, {'error': 'no such session'})
            if method == 'GET' and len(p) == 2:
                return self.send_json(200, {'header': s.header, 'events': s.events, 'status': s.status})
            if method == 'DELETE' and len(p) == 2:
                del SESSIONS[p[1]]
                return self.send_empty(204)
            if method == 'POST' and p[2:] == ['prompt']:
                req = json.loads(self.body() or b'{}')
                threading.Thread(target=self.canned_turn, args=(s, req), daemon=True).start()
                return self.send_empty(202)
            if method == 'POST' and p[2:] == ['cancel']:
                s.set_status('idle')
                return self.send_empty(202)
        if method == 'POST' and len(p) == 2 and p[0] == 'approvals':
            decision = json.loads(self.body() or b'{}').get('decision')
            for s in SESSIONS.values():
                if s.approval and s.approval['callId'] == p[1]:
                    s.approval = None
                    events = s.on_approval.get(decision, [])

                    def run(s=s, events=events):
                        s.set_status('running')
                        s.play(events)
                        s.set_status('idle')
                    threading.Thread(target=run, daemon=True).start()
                    return self.send_empty(204)
            return self.send_json(404, {'error': 'no pending approval'})
        if p == ['attachments'] and method == 'POST':
            data = self.body()
            att = f'att-{uuid.uuid4().hex[:12]}'
            ATTACHMENTS[att] = (data, self.headers.get('Content-Type', 'application/octet-stream'))
            return self.send_json(200, {'attachmentId': att, 'mediaType': ATTACHMENTS[att][1], 'bytes': len(data), 'width': 0, 'height': 0})
        if len(p) == 2 and p[0] == 'attachments' and method == 'GET':
            if p[1] not in ATTACHMENTS:
                return self.send_json(404, {'error': 'no such attachment'})
            data, mime = ATTACHMENTS[p[1]]
            self.send_response(200)
            self.send_header('Content-Type', mime)
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return None
        if p == ['stream'] and method == 'GET':
            return self.stream(query)
        return self.send_json(404, {'error': 'unknown route'})

    def canned_turn(self, s, req):
        turn = 1 + sum(1 for e in s.events if e['type'] == 'turn/start')
        content = [{'type': 'text', 'text': req.get('text', '')}] + [{'type': 'image', 'attachment': a} for a in req.get('images', [])]
        s.set_status('running')
        s.append({'type': 'turn/start', 'data': {'turn': turn}})
        s.append({'type': 'step/start', 'data': {'turn': turn, 'step': 1}})
        s.append({'type': 'user/message', 'data': {'role': 'user', 'id': str(uuid.uuid4()), 'source': {'kind': 'user'}, 'content': content}, 'surfaceOp': 'append'})
        if not s.title:
            s.append({'type': 'session/title', 'data': {'title': req.get('text', '')[:40], 'messageSeqs': [], 'source': {'kind': 'fallback'}}})
        s.play([
            {'type': 'assistant/message', 'data': {'turn': turn, 'step': 1, 'stream': [],
             'usage': {'inputTokens': 800, 'outputTokens': 30, 'cacheReadTokens': 2400},
             'message': {'role': 'assistant', 'id': str(uuid.uuid4()), 'source': {'kind': 'model', 'provider': 'deepseek-official', 'model': req.get('model', 'deepseek-flash')},
                         'content': [{'type': 'reasoning', 'text': 'This is the mock server.'},
                                     {'type': 'text', 'text': f"Mock server: got **{len(req.get('text', ''))}** characters and {len(req.get('images', []))} image(s) with model `{req.get('model')}`, effort `{req.get('effort')}`, sandbox `{req.get('sandbox')}`."}]}},
             'surfaceOp': 'append'},
            {'type': 'step/end', 'data': {'turn': turn, 'step': 1}},
            {'type': 'turn/end', 'data': {'turn': turn, 'reason': {'kind': 'completed'}}},
        ])
        s.set_status('idle')

    def stream(self, query):
        s = SESSIONS.get(query.get('session', [''])[0])
        if not s or self.headers.get('Upgrade', '').lower() != 'websocket':
            return self.send_json(400, {'error': 'bad stream request'})
        key = self.headers['Sec-WebSocket-Key']
        accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        self.send_response(101)
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', accept)
        self.end_headers()
        self.wfile.flush()
        after = int(query.get('after', ['-1'])[0])
        q = queue.Queue()
        with s.lock:
            backlog = [e for e in s.events if e['seq'] > after]
            s.subs.add(q)
        sock = self.connection
        try:
            for e in backlog:
                sock.sendall(ws_frame(json.dumps({'kind': 'event', 'sessionId': s.header['id'], 'event': e})))
            sock.sendall(ws_frame(json.dumps({'kind': 'status', 'sessionId': s.header['id'], 'status': s.status})))
            if s.pending or s.approval:
                threading.Thread(target=s.replay_tail, daemon=True).start()
            while True:
                r, _, _ = select.select([sock], [], [], 0.05)
                if r:
                    head = sock.recv(2)
                    if not head or head[0] & 0x0F == 0x8:
                        break
                    n = head[1] & 0x7F
                    if n == 126:
                        n = struct.unpack('!H', sock.recv(2))[0]
                    elif n == 127:
                        n = struct.unpack('!Q', sock.recv(8))[0]
                    sock.recv(4 + n)  # mask + payload; the UI never sends anything we need
                try:
                    while True:
                        sock.sendall(ws_frame(json.dumps(q.get_nowait())))
                except queue.Empty:
                    pass
        except OSError:
            pass
        finally:
            s.subs.discard(q)
            self.close_connection = True

    def do_GET(self):
        self.route('GET')

    def do_POST(self):
        self.route('POST')

    def do_PUT(self):
        self.route('PUT')

    def do_DELETE(self):
        self.route('DELETE')


if __name__ == '__main__':
    load()
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    srv = ThreadingHTTPServer(('127.0.0.1', port), Handler)
    print(f'http://127.0.0.1:{srv.server_address[1]}/?token={TOKEN}', flush=True)
    srv.serve_forever()
