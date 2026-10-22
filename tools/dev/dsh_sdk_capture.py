"""Drive the official `dsh --profile sdk-minimal` over JSON-RPC (stdio) for one prompt.

This is how docs/rust/upstream/*-minimal-0.2.0-rc.2.* were captured: the
sdk-minimal profile composes the same persona row (`includeHarnessIdentity:
false`, `includeRuntimeContext: false`, the fixed persona) and the same
persistent-bash row as the Minimal agent preset of dsh's web bundle.

    python3 tools/dev/mock_deepseek.py 18131 0 /dev/null DUMP_DIR &
    DSH_HOME=SCRATCH/dshhome DSH_AGENTS_HOME=SCRATCH/agents \
    DEEPSEEK_API_KEY=sk-mock DEEPSEEK_BASE_URL=http://127.0.0.1:18131/anthropic \
    python3 -I tools/dev/dsh_sdk_capture.py SCRATCH/ws

DSH_HOME keeps dsh away from ~/.dsh; the session (with its `request/header`
event) lands in DSH_HOME/sessions as plain JSONL. Never use a real key.
"""
import json, subprocess, sys, uuid

cwd = sys.argv[1]
proc = subprocess.Popen(['dsh', '--profile', 'sdk-minimal'], cwd=cwd,
                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr, text=True)
seq = [0]


def call(method, params):
    seq[0] += 1
    rid = seq[0]
    proc.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': rid, 'method': method, 'params': params}) + '\n')
    proc.stdin.flush()
    while True:
        line = proc.stdout.readline()
        if not line:
            raise SystemExit(f'dsh closed stdout during {method}')
        msg = json.loads(line)
        if msg.get('id') == rid:
            print(method, '->', json.dumps(msg)[:400], file=sys.stderr)
            return msg


call('initialize', {'cwd': cwd, 'provider': 'deepseek-official', 'model': 'deepseek-flash'})
sid = 'session-' + str(uuid.uuid4())
call('session/prompt', {'sessionId': sid, 'contentBlocks': [{'type': 'text', 'text': 'Say FERTIG.'}]})
# Notifications until the turn ends (or 30 lines without one).
for _ in range(200):
    line = proc.stdout.readline()
    if not line:
        break
    msg = json.loads(line)
    print('note', json.dumps(msg)[:300], file=sys.stderr)
    event = msg.get('params', {}).get('event', {})
    if event.get('type') == 'turn/end':
        break
call('shutdown', {})
proc.wait(timeout=20)
