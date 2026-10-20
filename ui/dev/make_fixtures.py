"""Builds ui/fixtures/*.json.

    python3 ui/dev/make_fixtures.py CAPTURE_DIR

CAPTURE_DIR holds session logs written by the official dsh 0.2.0-rc.2 against a
scripted mock DeepSeek (see SPEC §2), decompressed with zstdcat:
coding.jsonl, markdown.jsonl, error.jsonl. They are sanitized (scratch paths ->
/Users/alex/code/..., dsh telemetry receipts dropped, big prompt/header events
trimmed, seq renumbered) and re-timed to a believable pace.

approval, browser and computer are synthesized in the same envelope, because
headless dsh fails approvals closed and has no Mac tools; their tool/result
meta follows index.js presentationMeta.
"""
import json, pathlib, sys, uuid

ROOT = pathlib.Path(__file__).resolve().parent.parent / 'fixtures'
T0 = 1792487640000  # 2026-10-20 09:14:00 UTC
NS = uuid.UUID('6f1d9a52-2b7c-4f0e-9a51-1c2d3e4f5a6b')


def clamp(v, lo, hi):
    return max(lo, min(hi, v))


def replace_strings(value, pairs):
    if isinstance(value, str):
        for a, b in pairs:
            value = value.replace(a, b)
        return value
    if isinstance(value, list):
        return [replace_strings(v, pairs) for v in value]
    if isinstance(value, dict):
        return {k: replace_strings(v, pairs) for k, v in value.items()}
    return value


def block_durations(blocks):
    out = []
    for b in blocks:
        if b['type'] == 'reasoning':
            out.append(clamp(len(b['text']) * 55, 1800, 9000))
        elif b['type'] == 'text':
            out.append(clamp(len(b['text']) * 7, 400, 14000))
        else:
            out.append(250 + len(b.get('arguments', '')) * 2)
    return out


TOOL_MS = {'bash': 1400, 'read': 60, 'write': 90, 'edit': 90, 'glob': 140, 'grep': 180, 'todo_write': 20,
           'browser': 2200, 'computer': 1300, 'app': 1300, 'web_fetch': 700}


def retime(events, t0, turn_gap=45000):
    """Assigns a believable timeline and rewrites the assistant stream times to match."""
    t = t0
    names = {}
    first = True
    for e in events:
        typ, d = e['type'], e.get('data', {})
        if typ == 'turn/start':
            t += 0 if first else turn_gap
            first = False
        elif typ == 'step/start':
            t += 250
        elif typ == 'assistant/message':
            blocks = d['message']['content']
            spans, cur = [], t + 700
            for dur in block_durations(blocks):
                spans.append((cur, cur + dur))
                cur += dur
            for rec in d.get('stream', []):
                if rec.get('type') == 'chunk':
                    c = rec['chunk']
                    if c.get('type') == 'block-start':
                        rec['time'] = spans[c['index']][0]
                    elif c.get('type') == 'block-end':
                        rec['time'] = spans[c['index']][1]
                    else:
                        rec['time'] = cur
                elif 'index' in rec and rec['index'] < len(spans):
                    rec['time0'] = spans[rec['index']][0]
            t = cur + 30
        elif typ == 'tool/call':
            names[d['callId']] = d['name']
            t += 15
        elif typ == 'tool/result':
            t += TOOL_MS.get(names.get(d['message'].get('toolCallId'), ''), 300)
        elif typ == 'llm/retry-started':
            t += 600
        else:
            t += 5
        e['time'] = t
    return events


def renumber(events):
    remap = {}
    for i, e in enumerate(events):
        remap[e['seq']] = i
        e['seq'] = i
    for e in events:
        if 'sourceEventSeqs' in e:
            e['sourceEventSeqs'] = [remap[s] for s in e['sourceEventSeqs'] if s in remap]
        d = e.get('data', {})
        if isinstance(d, dict) and 'messageSeqs' in d:
            d['messageSeqs'] = [remap[s] for s in d['messageSeqs'] if s in remap]
    return events


def sanitize(path, fake_cwd, title=None):
    lines = [json.loads(l) for l in open(path)]
    header, events = lines[0], lines[1:]
    real_cwd = header['cwd']
    scratch_root = real_cwd.rsplit('/cap/', 1)[0]
    pairs = [(real_cwd, fake_cwd), (scratch_root, '/Users/alex/code')]
    header = replace_strings(header, pairs)
    header['createdAt'] = T0
    out = []
    for e in events:
        typ = e['type']
        if typ.startswith('session-log-deepseek/'):
            continue  # dsh's upload receipts; dsh-codex sends no session logs
        e = replace_strings(e, pairs)
        d = e.get('data', {})
        if typ == 'system/message':
            text = d['message']['content'][0]['text']
            d['message']['content'][0]['text'] = text.split('\n\n')[0] + '\n\n[… system prompt trimmed for the fixture …]'
        elif typ == 'request/header':
            d['header']['tools'] = [{'name': x['name']} for x in d['header'].get('tools', [])]
        elif typ == 'session/title-llm-request':
            d['system'] = d.get('system', '')[:80] + '…'
        elif typ == 'session/title' and title and d.get('source', {}).get('kind') == 'provider':
            d['title'] = title
        out.append(e)
    return header, renumber(out)


def first_index(events, typ):
    return next(i for i, e in enumerate(events) if e['type'] == typ)


# ---- synthesized logs ------------------------------------------------------------------

class Log:
    def __init__(self, sid, cwd, t0=T0):
        self.sid, self.cwd, self.t = sid, cwd, t0
        self.header = {'type': 'session', 'version': 4, 'id': sid, 'createdAt': t0, 'cwd': cwd, 'isSeeded': False, 'delegationDepth': 0}
        self.events = []
        self.calls = {}

    def uid(self, tag):
        return str(uuid.uuid5(NS, f'{self.sid}:{tag}:{len(self.events)}'))

    def add(self, typ, data, dt=5, **extra):
        self.t += dt
        e = {'type': typ, 'seq': len(self.events), 'time': self.t, 'data': data}
        e.update(extra)
        self.events.append(e)
        return e

    def preamble(self, preset='workspace-write'):
        self.add('permission/preset', {'preset': preset}, 0)
        self.add('sandbox/mode', {'mode': preset})
        self.add('approval/policy', {'policy': 'ask'})

    def start_turn(self, turn, text, title=None):
        self.add('turn/start', {'turn': turn})
        self.add('step/start', {'turn': turn, 'step': 1}, 60)
        if turn == 1:
            self.add('system/message', {'turn': 1, 'step': 1, 'message': {'role': 'system', 'content': [{'type': 'text',
                     'text': 'You are an AI agent powered by DeepSeek Harness.\n\n[… system prompt trimmed for the fixture …]'}],
                     'source': {'kind': 'system-prompt'}, 'id': self.uid('sys')}}, surfaceOp='append')
        self.add('user/message', {'content': [{'type': 'text', 'text': text}], 'source': {'kind': 'user'}, 'role': 'user', 'id': self.uid('user')},
                 surfaceOp='append')
        self.add('user/message', {'content': [{'type': 'text', 'text': f'Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\nCurrent DSH file policy: workspace-write. Session workspace: "{self.cwd}".'}],
                 'source': {'kind': 'runtime-context', 'form': 'snapshot', 'sections': []}, 'role': 'user', 'id': self.uid('ctx')}, surfaceOp='append')
        self.add('request/context', {'provider': 'deepseek-official', 'model': 'deepseek-flash', 'contextWindow': 1000000, 'systemPromptUpdate': 'in-history'})
        if title:
            self.add('session/title', {'title': text[:28], 'messageSeqs': [len(self.events) - 3], 'source': {'kind': 'fallback'}})
            self.add('session/title', {'title': title, 'messageSeqs': [len(self.events) - 4], 'source': {'kind': 'provider',
                     'provider': 'session-title-first-prompt-llm', 'model': {'provider': 'deepseek-official', 'model': 'deepseek-flash'}}}, 400)

    def step(self, turn, step):
        self.add('step/end', {'turn': turn, 'step': step - 1})
        self.add('step/start', {'turn': turn, 'step': step}, 250)

    def assistant(self, turn, step, blocks, usage=None):
        content = []
        for b in blocks:
            if b[0] == 'think':
                content.append({'type': 'reasoning', 'text': b[1]})
            elif b[0] == 'text':
                content.append({'type': 'text', 'text': b[1]})
            else:
                cid = 'toolu_' + uuid.uuid5(NS, f'{self.sid}:{len(self.events)}:{b[1]}:{len(content)}').hex[:10]
                content.append({'type': 'tool-call', 'id': cid, 'name': b[1], 'arguments': json.dumps(b[2])})
        spans, cur = [], self.t + 700
        for dur in block_durations(content):
            spans.append((cur, cur + dur))
            cur += dur
        stream = []
        for i, b in enumerate(content):
            kind = b['type']
            stream.append({'type': 'chunk', 'time': spans[i][0], 'chunk': {'type': 'block-start', 'index': i, 'blockType': kind}})
            if kind in ('reasoning', 'text'):
                stream.append({'type': f'{kind}-chunks', 'time0': spans[i][0], 'index': i, 'dt': [], 'texts': [b['text']]})
            stream.append({'type': 'chunk', 'time': spans[i][1], 'chunk': {'type': 'block-end', 'index': i, 'block': b}})
        usage = usage or {'inputTokens': 1840, 'outputTokens': 380, 'cacheReadTokens': 11264, 'totalTokens': 13484}
        stream.append({'type': 'chunk', 'time': cur, 'chunk': {'type': 'usage', 'usage': usage}})
        reason = 'tool-calls' if any(b['type'] == 'tool-call' for b in content) else 'stop'
        stream.append({'type': 'chunk', 'time': cur, 'chunk': {'type': 'finish', 'reason': {'kind': reason}}})
        self.t = cur
        msg = {'role': 'assistant', 'content': content, 'source': {'kind': 'model', 'provider': 'deepseek-official', 'model': 'deepseek-flash',
               'replayState': {'response': {'kind': 'deepseek-messages', 'version': 1, 'model': 'deepseek-flash'},
                               'blocks': [{'type': b['type'], **({'signature': 'sig'} if b['type'] == 'reasoning' else {})} for b in content]}},
               'id': self.uid('asst')}
        e = self.add('assistant/message', {'turn': turn, 'step': step, 'message': msg, 'usage': usage, 'stream': stream}, 30, surfaceOp='append')
        ids = []
        for b in content:
            if b['type'] == 'tool-call':
                self.calls[b['id']] = self.add('tool/call', {'turn': turn, 'step': step, 'callId': b['id'], 'name': b['name'], 'arguments': b['arguments']}, 10)
                ids.append(b['id'])
        return ids

    def result(self, turn, step, cid, content, meta=None, is_error=False, error=None):
        name = self.calls[cid]['data']['name']
        data = {'turn': turn, 'step': step, 'message': {'role': 'tool', 'source': {'kind': 'tool', 'callId': cid}, 'toolCallId': cid,
                'content': content, 'isError': is_error, 'id': self.uid('res')}}
        if error:
            data['error'] = error
        if meta is not None:
            data['meta'] = meta
        return self.add('tool/result', data, TOOL_MS.get(name, 300), sourceEventSeqs=[self.calls[cid]['seq']], surfaceOp='append')

    def end(self, turn, step, reason=None):
        self.add('step/end', {'turn': turn, 'step': step})
        self.add('turn/end', {'turn': turn, 'reason': reason or {'kind': 'completed'}})


def shot(att_id, name):
    size = (ROOT / 'img' / name).stat().st_size
    return {'type': 'image', 'attachment': {'attachmentId': att_id, 'mediaType': 'image/jpeg', 'bytes': size, 'width': 960, 'height': 600,
                                            'name': 'screen.jpg', 'originalDimensions': {'width': 1280, 'height': 800}}}


SCALE_NOTE = ' (The attached image is scaled to 960×600; keep using coordinates of the 1280×800 screenshot.)'
SCREEN = {'width': 1280, 'height': 800}


def meta(tool, args, label, cursor=None, place=None, target=None):
    return {'tool': tool, 'action': args['action'], 'label': label,
            'x': args.get('x'), 'y': args.get('y'), 'toX': args.get('to_x'), 'toY': args.get('to_y'),
            'screen': SCREEN, 'cursor': cursor, 'place': place, 'target': target}


def approval_fixture():
    L = Log('session-2b8e5d0c-7a41-4c9e-9f36-5e1a0c3d7b42', '/Users/alex/code/storefront', T0 + 3 * 3600_000)
    L.preamble()
    L.start_turn(1, 'Install the dependencies and run the test suite.', 'Install deps and run tests')
    live = len(L.events)
    [read] = L.assistant(1, 1, [('think', 'There is a package-lock.json, so npm ci is the right install. Check the scripts first.'),
                                ('tool', 'read', {'file_path': 'package.json'})])
    pkg = ['{', '  "name": "storefront",', '  "version": "1.4.0",', '  "private": true,', '  "type": "module",', '  "scripts": {',
           '    "dev": "vite",', '    "build": "vite build",', '    "test": "vitest run"', '  },', '  "devDependencies": {',
           '    "vite": "^7.1.0",', '    "vitest": "^4.0.2"', '  }', '}']
    L.result(1, 1, read, [{'type': 'text', 'text': '<path>/Users/alex/code/storefront/package.json</path>\n<type>file</type>\n<content>\n'
                           + '\n'.join(f'{i + 1}: {l}' for i, l in enumerate(pkg)) + f'\n\n(End of file - total {len(pkg)} lines)\n</content>'}],
             meta={'path': '/Users/alex/code/storefront/package.json', 'offset': 1, 'lines': [{'number': i + 1, 'text': l} for i, l in enumerate(pkg)],
                   'totalLines': len(pkg), 'lang': 'json'})
    L.step(1, 2)
    args = {'command': 'npm ci', 'description': 'Install dependencies', 'sandbox_permissions': 'danger-full-access',
            'justification': 'npm has to download packages from the registry, and the workspace sandbox blocks network access.'}
    [bash] = L.assistant(1, 2, [('text', 'The tests run with Vitest. Installing needs network access, so I am asking for it once.'), ('tool', 'bash', args)])
    asked = L.add('approval/asked', {'id': 'approval-1', 'toolName': 'bash', 'callId': bash, 'reason': args['justification']})
    sid = L.sid

    # What happens after either answer (seq/time are assigned during replay).
    def tail(decision):
        M = Log(sid, L.cwd, L.t)
        M.calls = dict(L.calls)
        M.events = [dict(e) for e in L.events]
        start = len(M.events)
        M.add('approval/decided', {'id': 'approval-1', 'outcome': decision})
        if decision == 'allowed-once':
            M.result(1, 2, bash, [{'type': 'text', 'text': 'added 214 packages, and audited 215 packages in 6s\n\n41 packages are looking for funding\n  run `npm fund` for details\n\nfound 0 vulnerabilities\n'}])
            M.step(1, 3)
            [test] = M.assistant(1, 3, [('tool', 'bash', {'command': 'npm test', 'description': 'Run the test suite'})])
            M.result(1, 3, test, [{'type': 'text', 'text': '\n> storefront@1.4.0 test\n> vitest run\n\n RUN  v4.0.2 /Users/alex/code/storefront\n\n ✓ src/cart.test.ts (6 tests) 12ms\n ✓ src/price.test.ts (4 tests) 5ms\n\n Test Files  2 passed (2)\n      Tests  10 passed (10)\n   Duration  1.21s\n'}])
            M.step(1, 4)
            M.assistant(1, 4, [('text', 'Dependencies are installed and all **10 tests** pass (`src/cart.test.ts`, `src/price.test.ts`).')])
            M.end(1, 4)
        else:
            M.result(1, 2, bash, [{'type': 'text', 'text': 'Error: The user did not allow this command to run with danger-full-access.'}], is_error=True,
                     error={'name': 'ApprovalError', 'code': 'APPROVAL_REJECTED', 'reason': 'Not allowed by the user'})
            M.step(1, 3)
            M.assistant(1, 3, [('text', 'Understood, I will not install anything. Run `npm ci` yourself when you are ready and I will take it from there.')])
            M.end(1, 3)
        return [{k: v for k, v in e.items() if k not in ('seq', 'time')} for e in M.events[start:]]

    return {
        'description': 'A bash call asks for full access; the approval card waits for an answer.',
        'session': {'header': L.header, 'events': L.events}, 'live': live,
        'approval': {'callId': bash, 'tool': 'bash', 'summary': 'npm ci', 'justification': args['justification']},
        'onApproval': {'allowed-once': tail('allowed-once'), 'rejected': tail('rejected')},
    }


def browser_fixture():
    L = Log('session-91c4e7a2-5d38-4b1f-8e60-3a9b2c7d1f05', '/Users/alex/code/dsh-codex', T0 + 2 * 3600_000)
    L.preamble()
    L.start_turn(1, 'Check the Brindle 2.4 release notes on example.org and tell me what changed in the CLI.', 'Brindle 2.4 CLI changes')
    live = len(L.events)
    [f0] = L.assistant(1, 1, [('think', 'Try a plain fetch of the changelog first; fall back to the browser if the page is rendered client-side.'),
                              ('tool', 'web_fetch', {'url': 'https://example.org/changelog'})])
    L.result(1, 1, f0, [{'type': 'text', 'text': '# Changelog · Brindle\n\nThis page needs JavaScript to show the release list.\n\n[Docs](https://example.org/docs) · [Guides](https://example.org/guides)'}])
    L.step(1, 2)
    a1 = {'action': 'open', 'url': 'https://example.org'}
    [c1] = L.assistant(1, 2, [('text', 'The changelog is rendered in the page, so I will use the background browser.'), ('tool', 'browser', a1)])
    outline = '\n'.join(['[1] link "Brindle" @(118,42)', '[2] link "Docs" @(228,42)', '[3] link "Guides" @(296,42)', '[4] link "Changelog" @(382,42)',
                         '[5] link "Community" @(482,42)', '[6] textbox "Search docs" @(1096,42)', '[7] link "Get started" @(124,428)', '[8] link "Install" @(238,428)'])
    L.result(1, 2, c1, [{'type': 'text', 'text': f'Opened https://example.org/ — "Brindle · Static sites from Markdown"\n{outline}{SCALE_NOTE}'},
                        shot('att-browser-home', 'browser-home.jpg')],
             meta=meta('browser', a1, 'Open https://example.org', place='example.org'))
    L.step(1, 3)
    a2 = {'action': 'type', 'index': 6, 'text': 'release notes 2.4', 'submit': True}
    [c2] = L.assistant(1, 3, [('tool', 'browser', a2)])
    L.result(1, 3, c2, [{'type': 'text', 'text': 'Typed into [6] and pressed Enter. Now at https://example.org/search?q=release+notes+2.4 — "Search · Brindle"\n'
                         '[1] link "Release notes 2.4" @(141,249)\n[2] link "Release notes 2.3" @(141,354)\n[3] link "Upgrading to 2.4" @(138,459)' + SCALE_NOTE},
                        shot('att-browser-search', 'browser-search.jpg')],
             meta=meta('browser', a2, 'Type “release notes 2.4” [6]', cursor={'x': 1096, 'y': 42}, place='example.org', target='Search docs'))
    L.step(1, 4)
    a3 = {'action': 'click', 'index': 1}
    [c3] = L.assistant(1, 4, [('tool', 'browser', a3)])
    L.result(1, 4, c3, [{'type': 'text', 'text': 'Clicked [1]. Now at https://example.org/changelog/2.4 — "Release notes 2.4 · Brindle"' + SCALE_NOTE},
                        shot('att-browser-article', 'browser-article.jpg')],
             meta=meta('browser', a3, 'Click [1]', cursor={'x': 141, 'y': 249}, place='example.org', target='Release notes 2.4'))
    L.step(1, 5)
    a4 = {'action': 'read'}
    [c4] = L.assistant(1, 5, [('tool', 'browser', a4)])
    L.result(1, 5, c4, [{'type': 'text', 'text': 'Release notes 2.4\nOctober 14 · 6 min read\n\nCLI\n- brindle check finds broken links and missing images before you publish.\n'
                         '- brindle serve --json prints build events as JSON lines for editors.\n- Incremental builds are up to 3× faster on large sites.\n\nConfig\n- output is now out_dir; the old key still works with a warning.'}],
             meta={**meta('browser', a4, 'Read'), 'screen': None, 'place': 'example.org'})
    L.step(1, 6)
    L.assistant(1, 6, [('think', 'Summarise the CLI section; mention the config rename since it can break builds.'),
                       ('text', 'Brindle **2.4** changes three things in the CLI:\n\n'
                                '- **`brindle check`** is new. It finds broken links and missing images before you publish.\n'
                                '- **`brindle serve --json`** prints build events as JSON lines, so editors can follow a build.\n'
                                '- **Incremental builds** are up to 3× faster on large sites.\n\n'
                                'One config key was renamed as well: `output` is now `out_dir`. The old key still works but prints a warning.')])
    L.end(1, 6)
    return {'description': 'The browser tool opens a site, searches, clicks a result and reads it.',
            'session': {'header': L.header, 'events': L.events}, 'live': live,
            'attachments': {'att-browser-home': 'img/browser-home.jpg', 'att-browser-search': 'img/browser-search.jpg',
                            'att-browser-article': 'img/browser-article.jpg'}}


def computer_fixture():
    sys.path.insert(0, str(pathlib.Path(__file__).parent))
    from frames import key_center  # same geometry the frames were drawn with
    L = Log('session-5e0b3f8d-1c27-4a96-b4d3-8f2e6a1c9d70', '/Users/alex', T0 + 26 * 3600_000)
    L.preamble('danger-full-access')
    L.start_turn(1, 'Open Calculator and work out 7 × 6.', 'Calculator: 7 × 6')
    live = len(L.events)
    steps = [
        ({'action': 'screenshot'}, 'att-desktop', 'desktop.jpg', None, 'Took a screenshot of the main display (1280×800).'),
        ({'action': 'open_app', 'text': 'Calculator'}, 'att-calc-0', 'calc-0.jpg', None, 'Opened Calculator and brought it to the front.'),
        ({'action': 'click', 'x': key_center('7')[0], 'y': key_center('7')[1]}, 'att-calc-7', 'calc-7.jpg', 'Calculator', 'Clicked at (550, 374).'),
        ({'action': 'click', 'x': key_center('×')[0], 'y': key_center('×')[1]}, 'att-calc-7x', 'calc-7x.jpg', 'Calculator', 'Clicked at (730, 374).'),
        ({'action': 'click', 'x': key_center('6')[0], 'y': key_center('6')[1]}, 'att-calc-6', 'calc-6.jpg', 'Calculator', 'Clicked at (670, 430).'),
        ({'action': 'click', 'x': key_center('=')[0], 'y': key_center('=')[1]}, 'att-calc-42', 'calc-42.jpg', 'Calculator', 'Clicked at (730, 542).'),
    ]
    labels = {'screenshot': 'Screenshot', 'open_app': 'Open Calculator'}
    for i, (args, att, img, place, text) in enumerate(steps, start=1):
        if i > 1:
            L.step(1, i)
        blocks = [('tool', 'computer', args)]
        if i == 1:
            blocks = [('think', 'Look at the screen first, then open Calculator and press 7, ×, 6, =.'), ('text', 'Taking a look at the screen first.')] + blocks
        if i == 3:
            blocks = [('think', 'Calculator is open and shows 0. The 7 key is in the second row on the left.')] + blocks
        [cid] = L.assistant(1, i, blocks)
        cursor = {'x': args['x'], 'y': args['y']} if 'x' in args else None
        label = labels.get(args['action'], f"Click ({args.get('x')}, {args.get('y')})")
        L.result(1, i, cid, [{'type': 'text', 'text': text + SCALE_NOTE}, shot(att, img)],
                 meta=meta('computer', args, label, cursor=cursor, place=place or ('Calculator' if i > 1 else None)))
    L.step(1, len(steps) + 1)
    L.assistant(1, len(steps) + 1, [('text', '7 × 6 = **42**. Calculator is still open on your screen.')])
    L.end(1, len(steps) + 1)
    atts = {att: f'img/{img}' for _, att, img, _, _ in steps}
    return {'description': 'The computer tool takes over the screen: screenshot, open an app, four clicks.',
            'session': {'header': L.header, 'events': L.events}, 'live': live, 'attachments': atts}


def captured(path, cwd, title=None, live_from='assistant/message', turn=1):
    header, events = sanitize(path, cwd, title)
    retime(events, header['createdAt'])
    starts = [i for i, e in enumerate(events) if e['type'] == 'turn/start']
    begin = starts[turn - 1] if turn - 1 < len(starts) else 0
    live = next(i for i in range(begin, len(events)) if events[i]['type'] == live_from)
    return {'header': header, 'events': events}, live


def main():
    cap = pathlib.Path(sys.argv[1])
    out = {}
    s, live = captured(cap / 'coding.jsonl', '/Users/alex/code/tally', 'Add --top flag to tally')
    out['coding'] = {'description': 'Real dsh log: plan, glob, read, grep, edit with diff, three bash runs (one fails), final answer.',
                     'session': s, 'live': live}
    s, live = captured(cap / 'markdown.jsonl', '/Users/alex/code/dsh-codex', 'Session log storage explained', turn=2)
    out['markdown'] = {'description': 'Real dsh log: two turns, the second a long Markdown answer with code blocks and a table.',
                       'session': s, 'live': live}
    s, live = captured(cap / 'error.jsonl', '/Users/alex/code/timeparse', 'Refactor date parsing', live_from='assistant/attempt')
    out['error'] = {'description': 'Real dsh log: a 503 is retried, then the provider rejects the API key (AUTH).',
                    'session': s, 'live': live}
    out['approval'] = approval_fixture()
    out['browser'] = browser_fixture()
    out['computer'] = computer_fixture()
    out['hero'] = {'description': 'New chat: start page with the workspace question.', 'session': None, 'cwd': '/Users/alex/code/tally'}
    out['nokey'] = {'description': 'First start: no API key yet, so a banner points to Settings.', 'session': None,
                    'cwd': '/Users/alex/code/tally', 'info': {'apiKeyConfigured': False}}

    minute = 60_000
    ago = {'computer': 4 * minute, 'approval': 38 * minute, 'browser': 2 * 60 * minute, 'markdown': 3 * 60 * minute,
           'coding': 26 * 60 * minute, 'error': 3 * 24 * 60 * minute}
    sessions = []
    for name, fx in out.items():
        if not fx.get('session'):
            continue
        h = fx['session']['header']
        title = next((e['data']['title'] for e in reversed(fx['session']['events']) if e['type'] == 'session/title'), '')
        sessions.append({'id': h['id'], 'title': title, 'cwd': h['cwd'], 'createdAt': h['createdAt'], 'updatedAgo': ago[name],
                         'status': 'error' if name == 'error' else 'idle', 'fixture': name})
    sessions.sort(key=lambda s: s['updatedAgo'])
    shared = {
        'description': 'Sidebar and workspace list shared by all fixtures. Each session opens its fixture.',
        'workspaces': [{'path': p, 'name': p.rsplit('/', 1)[-1], 'lastUsedAgo': i * 3600_000} for i, p in enumerate(
            ['/Users/alex/code/tally', '/Users/alex/code/dsh-codex', '/Users/alex/code/storefront', '/Users/alex/code/timeparse', '/Users/alex'])],
        'sessions': sessions,
    }
    ROOT.mkdir(exist_ok=True)
    (ROOT / '_shared.json').write_text(json.dumps(shared, indent=1, ensure_ascii=False) + '\n')
    for name, fx in out.items():
        (ROOT / f'{name}.json').write_text(json.dumps(fx, indent=1, ensure_ascii=False) + '\n')
        print(name, (ROOT / f'{name}.json').stat().st_size)


if __name__ == '__main__':
    main()
