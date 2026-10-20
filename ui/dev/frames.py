"""Renders the placeholder screenshots used by the browser and computer fixtures.

Run with the Playwright venv:  /root/.venvs/playwright/bin/python ui/dev/frames.py
Writes small JPEGs (960x600, the 1280x800 "screen" at 0.75 scale) to ui/fixtures/img/.
The pages are invented (a docs site on example.org and a plain desktop with a
calculator), so no real product or brand appears in them.
"""
import pathlib

OUT = pathlib.Path(__file__).resolve().parent.parent / 'fixtures' / 'img'
CHROME = '/root/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome'

BASE_CSS = """
* { box-sizing: border-box; margin: 0; padding: 0; }
body { width: 1280px; height: 800px; overflow: hidden; font-family: 'Lato', 'Liberation Sans', Arial, sans-serif; color: #1f2430; background: #fff; }
"""

SITE_HEAD = """
<header style="height:84px;display:flex;align-items:center;padding:0 64px;border-bottom:1px solid #e9ebf0;gap:40px">
  <div style="display:flex;align-items:center;gap:10px;font-weight:800;font-size:22px;letter-spacing:-.02em">
    <span style="width:30px;height:30px;border-radius:9px;background:linear-gradient(135deg,#7ad0c0,#2f8f83)"></span>Brindle</div>
  <nav style="display:flex;gap:28px;font-size:15px;color:#5a6172"><span style="color:#1f2430;font-weight:700">Docs</span><span>Guides</span><span>Changelog</span><span>Community</span></nav>
  <div style="margin-left:auto;width:240px;height:38px;border-radius:10px;border:1px solid {border};background:#f6f7f9;display:flex;align-items:center;padding:0 14px;gap:10px;color:{qc};font-size:14px;box-shadow:{ring}">
    <svg width="15" height="15" viewBox="0 0 16 16"><circle cx="7" cy="7" r="5" fill="none" stroke="#8a90a0" stroke-width="1.6"/><path d="m11 11 3.5 3.5" stroke="#8a90a0" stroke-width="1.6" stroke-linecap="round"/></svg>{q}</div>
</header>
"""

def head(q=None):
    return SITE_HEAD.format(q=q or 'Search docs', qc='#1f2430' if q else '#8a90a0',
                            border='#2f8f83' if q else '#e3e6ec', ring='0 0 0 3px #2f8f8333' if q else 'none')

BROWSER_HOME = head() + """
<main style="padding:72px 64px 0;display:grid;grid-template-columns:1.2fr 1fr;gap:56px">
  <div>
    <div style="font-size:14px;font-weight:700;color:#2f8f83;letter-spacing:.08em">BRINDLE 2.4</div>
    <h1 style="font-size:52px;line-height:1.05;letter-spacing:-.03em;margin-top:14px">Build static sites<br>from plain Markdown.</h1>
    <p style="font-size:18px;line-height:1.6;color:#5a6172;margin-top:22px;max-width:520px">Brindle turns a folder of notes into a fast website. One binary, no config to start, live reload while you write.</p>
    <div style="display:flex;gap:12px;margin-top:30px">
      <span style="padding:13px 22px;border-radius:10px;background:#1f2430;color:#fff;font-weight:700;font-size:15px">Get started</span>
      <span style="padding:13px 22px;border-radius:10px;border:1px solid #d9dde5;font-weight:700;font-size:15px">Install</span></div>
  </div>
  <div style="border-radius:16px;background:#14161c;padding:26px 28px;font:15px/1.8 'Liberation Mono',monospace;color:#c9d1e0;height:300px">
    <div><span style="color:#7d869a">$</span> brindle new notes</div>
    <div style="color:#7ee0a1">✓ created notes/</div>
    <div><span style="color:#7d869a">$</span> brindle serve --open</div>
    <div style="color:#7d869a">watching 42 files</div>
    <div style="color:#7ee0a1">✓ ready on http://localhost:4400</div>
  </div>
</main>
<section style="padding:56px 64px 0;display:grid;grid-template-columns:repeat(3,1fr);gap:20px">
  <div style="border:1px solid #e9ebf0;border-radius:14px;padding:22px"><b>Quick start</b><p style="color:#5a6172;margin-top:8px;font-size:14px">Your first site in two minutes.</p></div>
  <div style="border:1px solid #e9ebf0;border-radius:14px;padding:22px"><b>CLI reference</b><p style="color:#5a6172;margin-top:8px;font-size:14px">Every command and flag.</p></div>
  <div style="border:1px solid #e9ebf0;border-radius:14px;padding:22px"><b>Themes</b><p style="color:#5a6172;margin-top:8px;font-size:14px">Layouts, colours and fonts.</p></div>
</section>
"""

RESULT = """<div style="padding:18px 0;border-bottom:1px solid #eef0f4">
  <div style="font-size:13px;color:#8a90a0">{crumb}</div>
  <div style="font-size:20px;font-weight:700;color:{color};margin-top:4px">{title}</div>
  <div style="font-size:15px;color:#5a6172;margin-top:6px">{snippet}</div></div>"""

BROWSER_SEARCH = head('release notes 2.4') + """
<main style="padding:44px 0 0 64px;width:820px">
  <div style="font-size:14px;color:#8a90a0">3 results</div>
  <h1 style="font-size:28px;letter-spacing:-.02em;margin-top:6px">Results for “release notes 2.4”</h1>
  <div style="margin-top:14px">""" + RESULT.format(crumb='Changelog', color='#1f7a6f', title='Release notes 2.4',
      snippet='New <b>brindle check</b> command, faster incremental builds and a --json flag for serve.') + RESULT.format(
      crumb='Changelog', color='#1f2430', title='Release notes 2.3', snippet='Image pipeline, sitemap.xml and RSS feeds.') + RESULT.format(
      crumb='Guides', color='#1f2430', title='Upgrading to 2.4', snippet='Two config keys were renamed; everything else keeps working.') + """
  </div>
</main>
"""

BROWSER_ARTICLE = head() + """
<div style="display:grid;grid-template-columns:240px 1fr;height:716px">
  <aside style="border-right:1px solid #e9ebf0;padding:36px 28px;font-size:14px;color:#5a6172;line-height:2.3">
    <div style="font-weight:700;color:#1f2430">Changelog</div><div style="color:#1f7a6f;font-weight:700">2.4</div><div>2.3</div><div>2.2</div><div>2.1</div></aside>
  <main style="padding:40px 56px;max-width:860px">
    <div style="font-size:14px;color:#8a90a0">October 14 · 6 min read</div>
    <h1 style="font-size:40px;letter-spacing:-.03em;margin-top:8px">Release notes 2.4</h1>
    <h2 style="font-size:22px;margin-top:30px">CLI</h2>
    <ul style="margin:12px 0 0 22px;font-size:16px;line-height:1.9;color:#3a4152">
      <li><b>brindle check</b> finds broken links and missing images before you publish.</li>
      <li><b>brindle serve --json</b> prints build events as JSON lines for editors.</li>
      <li>Incremental builds are up to 3× faster on large sites.</li></ul>
    <h2 style="font-size:22px;margin-top:26px">Config</h2>
    <ul style="margin:12px 0 0 22px;font-size:16px;line-height:1.9;color:#3a4152">
      <li><code>output</code> is now <code>out_dir</code>; the old key still works with a warning.</li></ul>
  </main>
</div>
"""

DESKTOP = """
<div style="position:absolute;inset:0;background:radial-gradient(120% 90% at 20% 10%,#9ec5ff 0%,#7c8cf0 38%,#5d4fb8 70%,#33285e 100%)"></div>
<div style="position:absolute;left:0;right:0;top:0;height:28px;background:#ffffff59;backdrop-filter:blur(20px);display:flex;align-items:center;padding:0 16px;gap:20px;font-size:13.5px;color:#111">
  <span style="width:14px;height:14px;border-radius:4px;background:#111"></span><b>Calculator</b><span>File</span><span>Edit</span><span>View</span><span>Window</span><span>Help</span>
  <span style="margin-left:auto">Tue 20 Oct  09:41</span></div>
<div style="position:absolute;left:50%;bottom:10px;transform:translateX(-50%);height:66px;padding:8px 12px;border-radius:20px;background:#ffffff40;border:1px solid #ffffff60;display:flex;gap:10px">
  {dock}</div>
{window}
"""

DOCK = ''.join(f'<span style="width:50px;height:50px;border-radius:13px;background:{c}"></span>' for c in
               ['#4f8cff', '#ff9f43', '#2ecc71', '#e84393', '#a29bfe', '#636e72', '#fdcb6e'])

# Calculator geometry in screen pixels; the computer fixture clicks these centres.
CALC = dict(left=520, top=170, width=240, height=400, bar=28, display=92)
KEYS = [['AC', '±', '%', '÷'], ['7', '8', '9', '×'], ['4', '5', '6', '−'], ['1', '2', '3', '+'], ['0', '', '.', '=']]

def key_center(label):
    for r, row in enumerate(KEYS):
        for c, k in enumerate(row):
            if k == label:
                x = CALC['left'] + 30 + 60 * c
                y = CALC['top'] + CALC['bar'] + CALC['display'] + 28 + 56 * r
                return x, y
    raise KeyError(label)

def calculator(value, pressed=None):
    keys = []
    for r, row in enumerate(KEYS):
        for c, k in enumerate(row):
            if r == 4 and c == 1:
                continue
            w = 116 if (r == 4 and c == 0) else 56
            op = c == 3
            fn = r == 0 and c < 3
            bg = '#3d8bfd' if op else '#9aa3ad' if fn else '#4a4f57'
            if pressed == k:
                bg = '#9cc4ff' if op else '#6b717a'
            keys.append(f'<span style="position:absolute;left:{2 + 60 * c}px;top:{2 + 56 * r}px;width:{w}px;height:52px;border-radius:26px;'
                        f'background:{bg};color:{"#111" if fn else "#fff"};font-size:22px;display:flex;align-items:center;'
                        f'justify-content:{"flex-start;padding-left:22px" if w > 56 else "center"}">{k}</span>')
    L, T = CALC['left'], CALC['top']
    return f"""<div style="position:absolute;left:{L}px;top:{T}px;width:{CALC['width']}px;height:{CALC['height']}px;border-radius:14px;
      background:#2a2e35;box-shadow:0 30px 70px #0006,0 0 0 1px #0008;overflow:hidden">
      <div style="height:{CALC['bar']}px;display:flex;align-items:center;gap:7px;padding-left:12px">
        <span style="width:12px;height:12px;border-radius:6px;background:#ff5f57"></span><span style="width:12px;height:12px;border-radius:6px;background:#febc2e"></span><span style="width:12px;height:12px;border-radius:6px;background:#28c840"></span></div>
      <div style="height:{CALC['display']}px;display:flex;align-items:flex-end;justify-content:flex-end;padding:0 18px 6px;color:#fff;font-size:58px;font-weight:300">{value}</div>
      <div style="position:relative;height:280px">{''.join(keys)}</div></div>"""

def page(body):
    return f'<!doctype html><html><head><meta charset="utf-8"><style>{BASE_CSS}</style></head><body style="position:relative">{body}</body></html>'

FRAMES = {
    'browser-home.jpg': page(BROWSER_HOME),
    'browser-search.jpg': page(BROWSER_SEARCH),
    'browser-article.jpg': page(BROWSER_ARTICLE),
    'desktop.jpg': page(DESKTOP.format(dock=DOCK, window='')),
    'calc-0.jpg': page(DESKTOP.format(dock=DOCK, window=calculator('0'))),
    'calc-7.jpg': page(DESKTOP.format(dock=DOCK, window=calculator('7'))),
    'calc-7x.jpg': page(DESKTOP.format(dock=DOCK, window=calculator('7', pressed='×'))),
    'calc-6.jpg': page(DESKTOP.format(dock=DOCK, window=calculator('6'))),
    'calc-42.jpg': page(DESKTOP.format(dock=DOCK, window=calculator('42'))),
}

if __name__ == '__main__':
    from playwright.sync_api import sync_playwright
    OUT.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as p:
        b = p.chromium.launch(executable_path=CHROME)
        ctx = b.new_context(viewport={'width': 1280, 'height': 800}, device_scale_factor=0.75)
        pg = ctx.new_page()
        for name, markup in FRAMES.items():
            pg.set_content(markup)
            pg.screenshot(path=str(OUT / name), type='jpeg', quality=72)
            print(name, (OUT / name).stat().st_size)
        b.close()
    print('keys', {k: key_center(k) for k in ['7', '×', '6', '=']})
