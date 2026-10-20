"""Scripted DeepSeek stand-in (Anthropic Messages SSE) for golden references.

Usage: python3 mock_scripted.py PORT SCRIPT.json DUMP_DIR
SCRIPT is a list of responses, chosen by the number of assistant messages in the
request, or {"sequential": true, "responses": [...]} answered in request order
(across dsh processes, for resumed sessions); "fail_message" sets the error text.
Compaction requests answer a fixed summary. Each response is {"blocks": [...], "stop": "end_turn"|"tool_use"|"max_tokens",
"fail": [status, ...]} where blocks are {"thinking": "...", "signature": "..."},
{"text": "..."} or {"tool": "name", "args": "<raw json>", "id": "..."}. "fail"
lists HTTP statuses returned (one per attempt) before the success. "delay" sleeps
between chunks (seconds). Title requests answer "Testtitel".
"""
import json, sys, time, uuid
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

SCRIPT = json.load(open(sys.argv[2]))
DUMP = sys.argv[3]
COUNTER = [0]
FAILS = {}
MAIN = [0]


class H(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    disable_nagle_algorithm = True

    def log_message(self, *a):
        pass

    def do_GET(self):
        body = json.dumps({'data': []}).encode()
        self.send_response(200); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(body))); self.end_headers(); self.wfile.write(body)

    def do_POST(self):
        n = int(self.headers.get('content-length', 0)); req = json.loads(self.rfile.read(n) or b'{}')
        COUNTER[0] += 1
        with open(f'{DUMP}/{COUNTER[0]:03d}.json', 'w') as f:
            json.dump({'path': self.path, 'body': req}, f, indent=1, ensure_ascii=False)
        if not self.path.endswith('/messages'):
            self.send_response(200); self.send_header('content-type', 'application/json'); self.send_header('content-length', '2'); self.end_headers(); self.wfile.write(b'{}'); return
        sysp = json.dumps(req.get('system', ''))
        if 'concise title' in sysp:
            resp = {'blocks': [{'text': 'Testtitel'}], 'stop': 'end_turn'}
            idx = 'title'
        else:
            idx = sum(1 for m in req.get('messages', []) if m.get('role') == 'assistant')
            if 'compaction engine' in json.dumps(req.get('messages', [])[-1:]):
                resp = {'blocks': [{'text': '## Primary Request and Intent\n- summary'}], 'stop': 'end_turn'}
            elif isinstance(SCRIPT, dict):
                idx = MAIN[0]; MAIN[0] += 1
                resp = SCRIPT['responses'][min(idx, len(SCRIPT['responses']) - 1)]
            else:
                resp = SCRIPT[min(idx, len(SCRIPT) - 1)]
        fails = resp.get('fail', [])
        k = FAILS.get(idx, 0)
        if k < len(fails):
            FAILS[idx] = k + 1
            status = fails[k]
            msg = resp.get('fail_message', f'mock failure {status}')
            body = json.dumps({'type': 'error', 'error': {'type': 'invalid_request_error' if status == 400 else 'api_error', 'message': msg}}).encode()
            self.send_response(status); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(body))); self.end_headers(); self.wfile.write(body); return
        mid = 'msg_' + uuid.uuid4().hex[:12]
        ev = [('message_start', {'type': 'message_start', 'message': {'id': mid, 'type': 'message', 'role': 'assistant', 'model': req.get('model'), 'content': [], 'stop_reason': None, 'usage': {'input_tokens': 100, 'output_tokens': 1}}})]
        for i, b in enumerate(resp['blocks']):
            if 'thinking' in b:
                ev += [('content_block_start', {'type': 'content_block_start', 'index': i, 'content_block': {'type': 'thinking', 'thinking': ''}}),
                       ('content_block_delta', {'type': 'content_block_delta', 'index': i, 'delta': {'type': 'thinking_delta', 'thinking': b['thinking']}}),
                       ('content_block_delta', {'type': 'content_block_delta', 'index': i, 'delta': {'type': 'signature_delta', 'signature': b.get('signature', 'sig')}}),
                       ('content_block_stop', {'type': 'content_block_stop', 'index': i})]
            elif 'text' in b:
                parts = b.get('parts') or [b['text']]
                ev += [('content_block_start', {'type': 'content_block_start', 'index': i, 'content_block': {'type': 'text', 'text': ''}})]
                for p in parts:
                    ev += [('content_block_delta', {'type': 'content_block_delta', 'index': i, 'delta': {'type': 'text_delta', 'text': p}})]
                ev += [('content_block_stop', {'type': 'content_block_stop', 'index': i})]
            else:
                tid = b.get('id') or ('toolu_' + uuid.uuid4().hex[:12])
                ev += [('content_block_start', {'type': 'content_block_start', 'index': i, 'content_block': {'type': 'tool_use', 'id': tid, 'name': b['tool'], 'input': {}}}),
                       ('content_block_delta', {'type': 'content_block_delta', 'index': i, 'delta': {'type': 'input_json_delta', 'partial_json': b['args']}}),
                       ('content_block_stop', {'type': 'content_block_stop', 'index': i})]
        ev += [('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': resp.get('stop', 'end_turn')}, 'usage': {'output_tokens': 20}}),
               ('message_stop', {'type': 'message_stop'})]
        delay = resp.get('delay', 0)
        self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.send_header('cache-control', 'no-cache'); self.end_headers()
        try:
            for e, d in ev:
                self.wfile.write(f'event: {e}\ndata: {json.dumps(d)}\n\n'.encode()); self.wfile.flush()
                if delay:
                    time.sleep(delay)
        except BrokenPipeError:
            pass
        self.close_connection = True


ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
