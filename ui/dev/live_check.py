"""End-to-end check of api.js against the §6 mock server (real HTTP + WebSocket).

    /root/.venvs/playwright/bin/python ui/dev/live_check.py [--shots DIR]

Starts ui/dev/mock_server.py, opens the printed /?token=… URL in headless
Chromium and walks through: token handling, API key in Settings, a session
replayed over the WebSocket (deltas, events, images fetched with the bearer
token), an approval answered over HTTP, a new chat with an uploaded image,
and the 401 screen without a token. Exits 1 on any failure or console error.
"""
import argparse, json, pathlib, subprocess, sys
from playwright.sync_api import sync_playwright

UI = pathlib.Path(__file__).resolve().parent.parent
CHROME = '/root/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome'


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--shots')
    args = ap.parse_args()
    server = subprocess.Popen([sys.executable if 'venv' not in sys.executable else '/usr/bin/python3', str(UI / 'dev' / 'mock_server.py')],
                              stdout=subprocess.PIPE, text=True)
    url = server.stdout.readline().strip()
    base = url.split('/?')[0]
    results, problems = {}, []
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch(executable_path=CHROME)
            ctx = browser.new_context(viewport={'width': 1440, 'height': 900}, locale='en-US')
            page = ctx.new_page()
            page.on('console', lambda m: m.type in ('error', 'warning') and problems.append(f'console.{m.type}: {m.text}'))
            page.on('pageerror', lambda e: problems.append(f'pageerror: {e}'))

            page.goto(url)
            page.wait_for_selector('.sb-session')
            results['tokenStripped'] = 'token=' not in page.url
            results['noKeyBanner'] = page.is_visible('text=No DeepSeek API key yet')

            # API key through Settings (PUT /api/settings).
            page.click('.sb-foot .nav-item')
            page.fill('.key-row input', 'sk-test-not-a-real-key')
            page.click('.key-row .btn-primary')
            page.wait_for_selector('.key-state')
            page.click('.back-btn')
            results['keySaved'] = not page.is_visible('text=No DeepSeek API key yet')

            # A session whose tail is streamed over the WebSocket.
            page.click('.sb-session:has-text("Brindle 2.4 CLI changes")')
            page.wait_for_function("document.body.innerText.includes('One config key was renamed')", timeout=30000)
            page.wait_for_function("[...document.querySelectorAll('.cx-shot img')].length >= 3", timeout=10000)
            results['blobImages'] = page.evaluate("[...document.querySelectorAll('.cx-shot img')].every(i => i.src.startsWith('blob:') && i.naturalWidth > 0)")
            results['browserRows'] = page.locator('.cx-card[data-tool=browser]').count()
            if args.shots:
                page.screenshot(path=f'{args.shots}/live-browser.png')

            # Approval answered over HTTP, the rest arrives on the stream.
            page.click('.sb-session:has-text("Install deps and run tests")')
            page.wait_for_selector('.approval', timeout=30000)
            page.click('.approval .btn:has-text("Deny")')
            page.wait_for_function("document.body.innerText.includes('Understood, I will not install')", timeout=30000)
            results['approvalDenied'] = page.is_visible('.badge:has-text("Denied")') or page.is_visible('.is-error')

            # New chat with an uploaded image (POST /api/attachments, /api/sessions, /prompt).
            page.click('.sb-nav .nav-item')
            page.set_input_files('.composer input[type=file]', str(UI / 'fixtures' / 'img' / 'calc-42.jpg'))
            page.wait_for_selector('.composer-images .thumb img')
            page.fill('.composer-input', 'What does the calculator show?')
            page.press('.composer-input', 'Enter')
            page.wait_for_function("document.body.innerText.includes('Mock server: got') && !document.querySelector('.send.is-stop')", timeout=20000)
            reply = page.inner_text('.turn .md >> nth=-1')
            results['promptReply'] = reply
            results['promptOk'] = '1 image(s)' in reply and 'deepseek-flash' in reply and page.evaluate("location.hash.startsWith('#/s/')")
            results['userImageBlob'] = page.evaluate("!!document.querySelector('.user-images img[src^=\"blob:\"]')")
            if args.shots:
                page.screenshot(path=f'{args.shots}/live-prompt.png')
            ctx.close()

            # Without a token: the 401 screen (the failed request itself is expected).
            ctx = browser.new_context(viewport={'width': 1440, 'height': 900}, locale='en-US')
            page = ctx.new_page()
            page.goto(base + '/')
            page.wait_for_selector('.boot-error')
            results['unauthorizedScreen'] = 'access token' in page.inner_text('.boot-error')
            ctx.close()
            browser.close()
    finally:
        server.terminate()
    ok = all(v for k, v in results.items() if k != 'promptReply') and not problems
    print(json.dumps({'ok': ok, 'results': results, 'problems': problems}, indent=1))
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    main()
