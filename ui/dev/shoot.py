"""Screenshots and checks for every fixture, light and dark, desktop and phone.

    /root/.venvs/playwright/bin/python ui/dev/shoot.py [--only NAME] [--out DIR]

Serves ui/ with python3 -m http.server on a free port, opens
index.html?fixture=NAME&speed=0&theme=T in headless Chromium, saves
screenshots (palette PNG via the system python's Pillow when available),
fails on any console error, page error or failed request, and checks two
live behaviours with speed=1: the agent cursor glides and ripples in the
computer fixture, and the approval card answers and finishes the turn.
"""
import argparse, json, pathlib, socket, subprocess, sys, time
from playwright.sync_api import sync_playwright

UI = pathlib.Path(__file__).resolve().parent.parent
CHROME = '/root/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome'
FIXTURES = ['hero', 'coding', 'markdown', 'approval', 'browser', 'computer', 'error', 'nokey']
SIZES = {'desktop': (1440, 900), 'phone': (390, 844)}


def free_port():
    s = socket.socket()
    s.bind(('127.0.0.1', 0))
    port = s.getsockname()[1]
    s.close()
    return port


class Watch:
    """Collects everything that would show up red in DevTools."""

    def __init__(self, page, label):
        self.problems = []
        page.on('console', lambda m: m.type in ('error', 'warning') and self.problems.append(f'{label}: console.{m.type}: {m.text}'))
        page.on('pageerror', lambda e: self.problems.append(f'{label}: pageerror: {e}'))
        page.on('requestfailed', lambda r: self.problems.append(f'{label}: request failed: {r.url} {r.failure}'))
        page.on('response', lambda r: r.status >= 400 and self.problems.append(f'{label}: HTTP {r.status} {r.url}'))


def settle(page):
    page.wait_for_selector('.app')
    page.wait_for_load_state('networkidle')
    page.evaluate('document.fonts.ready')
    page.wait_for_function('[...document.images].every(i => i.complete)')
    page.wait_for_timeout(450)


def shots(base, out, only):
    problems = []
    files = []
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=CHROME)
        for name in FIXTURES:
            if only and name not in only:
                continue
            for theme in ('light', 'dark'):
                for size, (w, h) in SIZES.items():
                    ctx = browser.new_context(viewport={'width': w, 'height': h}, color_scheme=theme,
                                              device_scale_factor=1, reduced_motion='reduce')
                    page = ctx.new_page()
                    watch = Watch(page, f'{name}/{theme}/{size}')
                    page.goto(f'{base}/index.html?fixture={name}&speed=0&theme={theme}&lang=en')
                    settle(page)
                    path = out / f'{name}-{theme}-{size}.png'
                    page.screenshot(path=str(path))
                    files.append(path)
                    if name == 'coding' and size == 'desktop':
                        # The middle of the turn: read, grep, the edit diff and the plan.
                        page.evaluate("""() => { const s = document.querySelector('.scroll');
                          const row = [...document.querySelectorAll('.tool')].find(t => t.textContent.includes('Edited'));
                          s.scrollTop = row.offsetTop - 160; }""")
                        page.click('text=python3 -m unittest -q test_tally')
                        page.wait_for_timeout(200)
                        path = out / f'coding-diff-{theme}-{size}.png'
                        page.screenshot(path=str(path))
                        files.append(path)
                    if name == 'hero' and size == 'desktop':
                        page.click('.chip-model')
                        page.wait_for_timeout(200)
                        path = out / f'menu-model-{theme}-{size}.png'
                        page.screenshot(path=str(path))
                        files.append(path)
                        page.keyboard.press('Escape')
                        page.click('.sb-foot .nav-item')
                        settle(page)
                        path = out / f'settings-{theme}-{size}.png'
                        page.screenshot(path=str(path))
                        files.append(path)
                    if name == 'hero' and size == 'phone':
                        page.click('.topbar-toggle')
                        page.wait_for_timeout(400)
                        path = out / f'sidebar-{theme}-{size}.png'
                        page.screenshot(path=str(path))
                        files.append(path)
                    problems += watch.problems
                    ctx.close()
        browser.close()
    return files, problems


def check_cursor(base):
    """speed=1: the cursor must move between frames and a click must ripple."""
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=CHROME)
        page = browser.new_page(viewport={'width': 1440, 'height': 900})
        watch = Watch(page, 'cursor')
        page.goto(f'{base}/index.html?fixture=computer&speed=1&lang=en')
        page.wait_for_selector('.app')
        positions, ripples = [], 0
        deadline = time.time() + 40
        while time.time() < deadline:
            state = page.evaluate("""() => {
              const c = [...document.querySelectorAll('.cx-cursor')].pop();
              const g = c && c.querySelector('.cx-glyph');
              return { pos: c ? [c.style.left, c.style.top, g.style.transform] : null,
                       ripple: document.querySelectorAll('.cx-ripple').length,
                       done: !!document.querySelector('.turn .md') && document.body.innerText.includes('= 42') };
            }""")
            if state['pos']:
                positions.append(tuple(state['pos']))
            ripples = max(ripples, state['ripple'])
            if state['done']:
                break
            page.wait_for_timeout(40)
        browser.close()
    distinct = len(set(positions))
    return {'samples': len(positions), 'distinctPositions': distinct, 'ripples': ripples,
            'ok': distinct > 10 and ripples > 0, 'problems': watch.problems}


def check_approval(base, out):
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=CHROME)
        page = browser.new_page(viewport={'width': 1440, 'height': 900})
        watch = Watch(page, 'approval-live')
        page.goto(f'{base}/index.html?fixture=approval&speed=4&lang=en')
        page.wait_for_selector('.approval', timeout=30000)
        page.click('.approval .btn-primary')
        page.wait_for_function("document.body.innerText.includes('10 tests')", timeout=30000)
        page.wait_for_timeout(600)
        ok = page.evaluate("!document.querySelector('.approval') && !!document.querySelector('.worked')")
        page.screenshot(path=str(out / 'approval-allowed-light-desktop.png'))
        browser.close()
    return {'ok': ok, 'problems': watch.problems}


def check_prompt(base, out):
    """Start page → attach an image, type, Enter → a new session streams the fixture reply."""
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=CHROME)
        page = browser.new_page(viewport={'width': 1440, 'height': 900})
        watch = Watch(page, 'prompt')
        page.goto(f'{base}/index.html?fixture=hero&speed=2&lang=en')
        page.wait_for_selector('.composer-input')
        page.set_input_files('.composer input[type=file]', str(UI / 'fixtures' / 'img' / 'desktop.jpg'))
        page.wait_for_selector('.composer-images .thumb img')
        page.fill('.composer-input', 'What is on my screen right now?')
        page.press('.composer-input', 'Enter')
        page.wait_for_function("document.body.innerText.includes('Fixture mode')", timeout=20000)
        page.wait_for_function("!document.querySelector('.send.is-stop')", timeout=20000)
        page.wait_for_timeout(400)
        ok = page.evaluate("""() => !!document.querySelector('.user-images .thumb img')
          && document.querySelector('.bubble')?.textContent.includes('What is on my screen')
          && [...document.querySelectorAll('.sb-title')].some(e => e.textContent.includes('What is on my screen'))
          && location.hash.startsWith('#/s/')""")
        page.screenshot(path=str(out / 'prompt-roundtrip-light-desktop.png'))
        browser.close()
    return {'ok': ok, 'problems': watch.problems}


def german(base, out):
    files, problems = [], []
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=CHROME)
        for name, theme in (('coding', 'light'), ('hero', 'dark')):
            ctx = browser.new_context(viewport={'width': 1440, 'height': 900}, color_scheme=theme, reduced_motion='reduce', locale='de-DE')
            page = ctx.new_page()
            watch = Watch(page, f'{name}/de')
            page.goto(f'{base}/index.html?fixture={name}&speed=0&theme={theme}')
            settle(page)
            path = out / f'{name}-de-{theme}-desktop.png'
            page.screenshot(path=str(path))
            files.append(path)
            problems += watch.problems
            ctx.close()
        browser.close()
    return files, problems


def quantize(files):
    """Palette PNGs are 3-4x smaller; Pillow lives in the system python, not the venv."""
    code = ("import sys\nfrom PIL import Image\n"
            "for f in sys.argv[1:]:\n"
            "    im = Image.open(f).convert('RGB')\n"
            "    im.quantize(colors=256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(f, optimize=True)\n")
    try:
        subprocess.run(['/usr/bin/python3', '-c', code, *map(str, files)], check=True)
    except Exception as e:  # keep the full-colour PNGs
        print('quantize skipped:', e)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--only', nargs='*')
    ap.add_argument('--out', default=str(UI / 'screenshots'))
    ap.add_argument('--no-live', action='store_true')
    args = ap.parse_args()
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    port = free_port()
    server = subprocess.Popen([sys.executable, '-m', 'http.server', str(port), '--bind', '127.0.0.1'], cwd=UI,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        time.sleep(0.6)
        base = f'http://127.0.0.1:{port}'
        files, problems = shots(base, out, args.only)
        report = {'problems': problems}
        de_files, de_problems = german(base, out)
        files += de_files
        problems += de_problems
        if not args.no_live:
            cursor = check_cursor(base)
            approval = check_approval(base, out)
            prompt = check_prompt(base, out)
            files += [out / 'approval-allowed-light-desktop.png', out / 'prompt-roundtrip-light-desktop.png']
            report['cursor'] = cursor
            report['approval'] = approval
            report['prompt'] = prompt
            problems += cursor['problems'] + approval['problems'] + prompt['problems']
        quantize(files)
        report['screenshots'] = len(files)
        print(json.dumps(report, indent=1))
        live_ok = args.no_live or all(report[k]['ok'] for k in ('cursor', 'approval', 'prompt'))
        sys.exit(1 if problems or not live_ok else 0)
    finally:
        server.terminate()


if __name__ == '__main__':
    main()
