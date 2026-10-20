"""Instant DeepSeek stand-in (Anthropic Messages API, SSE) for offline tests.

Usage: python3 mock_deepseek.py PORT [N_BASH_CALLS | SCRIPT.json] [LOGFILE] [DUMP_DIR]
Point a harness at it with DEEPSEEK_BASE_URL=http://127.0.0.1:PORT/anthropic and
any DEEPSEEK_API_KEY. The number of tool results already in the history decides
the next step: N bash calls (date, hostname, ...), then the text "FERTIG".
SCRIPT.json is a list of tool calls `[{"name": "browser", "input": {...}}, ...]`
made one per step instead of the bash commands.
With DUMP_DIR every request body is saved as NNN.json, which is how the exact
wire format of the official dsh was captured for docs/rust/upstream/.
"""
import json, sys, time, uuid
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
CMDS = ['date', 'hostname', 'uname -r', 'pwd', 'whoami']
ARG = sys.argv[2] if len(sys.argv) > 2 else '0'
SCRIPT = json.load(open(ARG)) if ARG.endswith('.json') else None
N_TOOLS = len(SCRIPT) if SCRIPT is not None else int(ARG)
LOG = open(sys.argv[3] if len(sys.argv) > 3 else '/dev/null', 'a')
DUMP = sys.argv[4] if len(sys.argv) > 4 else None
COUNTER = [0]

class H(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    disable_nagle_algorithm = True  # sonst 40 ms Delayed-ACK-Pause bei wiederverwendeten Verbindungen
    def log_message(self, *a): pass
    def do_GET(self):
        LOG.write(f'GET {self.path}\n'); LOG.flush()
        body = json.dumps({'data': []}).encode()
        self.send_response(200); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_POST(self):
        n = int(self.headers.get('content-length', 0)); req = json.loads(self.rfile.read(n) or b'{}')
        LOG.write(f'{time.time():.3f} POST {self.path} stream={req.get("stream")} msgs={len(req.get("messages", []))}\n'); LOG.flush()
        if DUMP:
            COUNTER[0] += 1
            with open(f'{DUMP}/{COUNTER[0]:03d}.json', 'w') as f:
                json.dump({'path': self.path, 'headers': {k: v for k, v in self.headers.items() if k.lower() not in ('x-api-key', 'authorization')}, 'body': req}, f, indent=1, ensure_ascii=False)
        if not self.path.endswith('/messages'):
            body = b'{}'; self.send_response(200); self.send_header('content-type','application/json'); self.send_header('content-length','2'); self.end_headers(); self.wfile.write(body); return
        sysp = json.dumps(req.get('system', ''))
        done = sum(1 for m in req.get('messages', []) if isinstance(m.get('content'), list) for b in m['content'] if b.get('type') == 'tool_result')
        if 'concise title' in sysp: block = ('text', 'Testtitel')
        elif done < N_TOOLS: block = ('tool', SCRIPT[done] if SCRIPT is not None else {'name': 'bash', 'input': {'command': CMDS[done % len(CMDS)], 'description': 'run ' + CMDS[done % len(CMDS)]}})
        else: block = ('text', 'FERTIG')
        mid = 'msg_' + uuid.uuid4().hex[:12]
        ev = [('message_start', {'type': 'message_start', 'message': {'id': mid, 'type': 'message', 'role': 'assistant', 'model': req.get('model'), 'content': [], 'stop_reason': None, 'usage': {'input_tokens': 100, 'output_tokens': 1}}})]
        if block[0] == 'text':
            ev += [('content_block_start', {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}}),
                   ('content_block_delta', {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': block[1]}}),
                   ('content_block_stop', {'type': 'content_block_stop', 'index': 0}),
                   ('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'output_tokens': 5}})]
        else:
            args = json.dumps(block[1]['input'])
            ev += [('content_block_start', {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'tool_use', 'id': 'toolu_' + uuid.uuid4().hex[:12], 'name': block[1]['name'], 'input': {}}}),
                   ('content_block_delta', {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': args}}),
                   ('content_block_stop', {'type': 'content_block_stop', 'index': 0}),
                   ('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use'}, 'usage': {'output_tokens': 20}})]
        ev.append(('message_stop', {'type': 'message_stop'}))
        if req.get('stream'):
            data = ''.join(f'event: {e}\ndata: {json.dumps(d)}\n\n' for e, d in ev).encode()
            self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.send_header('content-length', str(len(data))); self.end_headers(); self.wfile.write(data)
        else:
            content = [{'type': 'text', 'text': block[1]}] if block[0] == 'text' else [{'type': 'tool_use', 'id': 'toolu_x', 'name': block[1]['name'], 'input': block[1]['input']}]
            body = json.dumps({'id': mid, 'type': 'message', 'role': 'assistant', 'model': req.get('model'), 'content': content, 'stop_reason': 'end_turn' if block[0]=='text' else 'tool_use', 'usage': {'input_tokens': 100, 'output_tokens': 5}}).encode()
            self.send_response(200); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(body))); self.end_headers(); self.wfile.write(body)

ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
