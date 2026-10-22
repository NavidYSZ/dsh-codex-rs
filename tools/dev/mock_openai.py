"""Instant OpenAI Chat Completions stand-in (SSE) for offline tests of custom providers.

Usage: python3 mock_openai.py PORT [N_BASH_CALLS] [LOGFILE] [DUMP_DIR]
Add it in Settings -> Models as a custom API with protocol "OpenAI Chat Completions",
base URL http://127.0.0.1:PORT/v1 and any key. GET /v1/models lists a few models.
The number of tool results already in the history decides the next step: N bash
calls (date, hostname, ...), then the text "FERTIG". Every answer streams a short
`reasoning_content` first and ends with usage whose prompt tokens are 80 % cached,
like a provider with a warm prefix cache. With DUMP_DIR every request body is
saved as NNN.json (keys stripped from the headers).
"""
import json, sys, time, uuid
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
CMDS = ['date', 'hostname', 'uname -r', 'pwd', 'whoami']
N_TOOLS = int(sys.argv[2]) if len(sys.argv) > 2 else 0
LOG = open(sys.argv[3] if len(sys.argv) > 3 else '/dev/null', 'a')
DUMP = sys.argv[4] if len(sys.argv) > 4 else None
COUNTER = [0]
MODELS = [
    {'id': 'glm-5.3', 'object': 'model', 'owned_by': 'mock', 'context_length': 1000000},
    {'id': 'glm-5.3-flash', 'object': 'model', 'owned_by': 'mock', 'context_length': 1000000,
     'architecture': {'input_modalities': ['text', 'image']}},
    {'id': 'kimi-k3', 'object': 'model', 'owned_by': 'mock', 'context_length': 1048576},
]

def chunk(delta, finish=None):
    return {'id': 'chatcmpl-' + uuid.uuid4().hex[:8], 'object': 'chat.completion.chunk', 'created': int(time.time()),
            'choices': [{'index': 0, 'delta': delta, 'finish_reason': finish}]}

class H(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    disable_nagle_algorithm = True
    def log_message(self, *a): pass
    def send_json(self, status, obj):
        body = json.dumps(obj).encode()
        self.send_response(status); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        LOG.write(f'GET {self.path}\n'); LOG.flush()
        if self.path.split('?')[0].endswith('/models'):
            return self.send_json(200, {'object': 'list', 'data': MODELS})
        self.send_json(404, {'error': {'message': 'unknown path', 'type': 'invalid_request_error'}})
    def do_POST(self):
        n = int(self.headers.get('content-length', 0)); req = json.loads(self.rfile.read(n) or b'{}')
        LOG.write(f'{time.time():.3f} POST {self.path} stream={req.get("stream")} msgs={len(req.get("messages", []))}\n'); LOG.flush()
        if DUMP:
            COUNTER[0] += 1
            with open(f'{DUMP}/{COUNTER[0]:03d}.json', 'w') as f:
                json.dump({'path': self.path, 'headers': {k: v for k, v in self.headers.items() if k.lower() not in ('x-api-key', 'authorization')}, 'body': req}, f, indent=1, ensure_ascii=False)
        if not self.path.endswith('/chat/completions'):
            return self.send_json(404, {'error': {'message': 'unknown path', 'type': 'invalid_request_error'}})
        messages = req.get('messages', [])
        done = sum(1 for m in messages if m.get('role') == 'tool')
        system = ' '.join(m.get('content') or '' for m in messages if m.get('role') == 'system' and isinstance(m.get('content'), str))
        events = [chunk({'role': 'assistant', 'reasoning_content': 'Looking at the request.'})]
        if 'concise title' in system:
            events = [chunk({'role': 'assistant', 'content': 'Testtitel'}), chunk({}, 'stop')]
        elif done < N_TOOLS:
            cmd = CMDS[done % len(CMDS)]
            args = json.dumps({'command': cmd, 'description': 'run ' + cmd})
            call = 'call_' + uuid.uuid4().hex[:12]
            events += [chunk({'tool_calls': [{'index': 0, 'id': call, 'type': 'function', 'function': {'name': 'bash', 'arguments': args[:10]}}]}),
                       chunk({'tool_calls': [{'index': 0, 'function': {'arguments': args[10:]}}]}),
                       chunk({}, 'tool_calls')]
        else:
            events += [chunk({'content': 'FER'}), chunk({'content': 'TIG'}), chunk({}, 'stop')]
        prompt = 1000 + 100 * len(messages)
        usage = {'id': 'usage', 'object': 'chat.completion.chunk', 'choices': [],
                 'usage': {'prompt_tokens': prompt, 'completion_tokens': 20, 'total_tokens': prompt + 20,
                           'prompt_tokens_details': {'cached_tokens': prompt * 8 // 10},
                           'completion_tokens_details': {'reasoning_tokens': 6}}}
        if (req.get('stream_options') or {}).get('include_usage'):
            events.append(usage)
        if not req.get('stream'):
            return self.send_json(400, {'error': {'message': 'mock_openai only streams', 'type': 'invalid_request_error'}})
        data = (''.join(f'data: {json.dumps(e)}\n\n' for e in events) + 'data: [DONE]\n\n').encode()
        self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.send_header('content-length', str(len(data))); self.end_headers(); self.wfile.write(data)

if __name__ == '__main__':
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18123
    print(f'mock_openai on http://127.0.0.1:{port}/v1', file=sys.stderr, flush=True)
    ThreadingHTTPServer(('127.0.0.1', port), H).serve_forever()
