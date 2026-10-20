//! In-page JavaScript, verbatim from `lib/browser.js` and `lib/browser-tool.js`
//! (template-literal escapes resolved: `\\s` there is `\s` here).

/// Indexes visible interactive elements (`data-dsh-ix`) and reports what the
/// model needs to act on them.
pub(crate) const SNAPSHOT: &str = r#"(() => {
  const SEL = 'a[href],button,input:not([type=hidden]),textarea,select,summary,label[for],[role=button],[role=link],' +
    '[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=option],[role=switch],[role=combobox],[role=textbox],' +
    '[role=searchbox],[contenteditable=""],[contenteditable=true],[onclick],[tabindex]:not([tabindex="-1"])';
  for (const e of document.querySelectorAll('[data-dsh-ix]')) e.removeAttribute('data-dsh-ix');
  const W = innerWidth, H = innerHeight, items = [];
  const clean = (s) => String(s || '').replace(/\s+/g, ' ').trim();
  for (const el of document.querySelectorAll(SEL)) {
    if (items.length >= 150) break;
    const r = el.getBoundingClientRect();
    if (r.width < 3 || r.height < 3 || r.bottom <= 0 || r.right <= 0 || r.top >= H || r.left >= W) continue;
    const st = getComputedStyle(el);
    if (st.visibility === 'hidden' || st.display === 'none' || Number(st.opacity) === 0) continue;
    const x = Math.min(Math.max(r.left + r.width / 2, 1), W - 1), y = Math.min(Math.max(r.top + r.height / 2, 1), H - 1);
    const hit = document.elementFromPoint(x, y);
    if (hit && hit !== el && !el.contains(hit) && !hit.contains(el)) continue;
    const tag = el.tagName.toLowerCase(), type = (el.getAttribute('type') || '').toLowerCase(), role = el.getAttribute('role');
    const field = tag === 'input' || tag === 'textarea' || tag === 'select';
    let name = clean(el.getAttribute('aria-label')) || clean(el.labels && el.labels[0] && el.labels[0].innerText) ||
      (field ? clean(el.getAttribute('placeholder')) : '') || clean(el.innerText) || clean(el.getAttribute('title')) ||
      clean(el.getAttribute('alt')) || (field ? '' : clean(el.value));
    if (!name && tag === 'a') name = clean(el.getAttribute('href')).slice(0, 60);
    if (!name) { const img = el.querySelector('img[alt],svg[aria-label]'); name = clean(img && (img.getAttribute('alt') || img.getAttribute('aria-label'))); }
    const item = { i: items.length, kind: role || (tag === 'input' ? 'input' + (type ? '[' + type + ']' : '') : tag), name: name.slice(0, 80), x: Math.round(x), y: Math.round(y) };
    if (field) { if (type === 'password') item.password = true; else if (type !== 'checkbox' && type !== 'radio') item.value = clean(el.value).slice(0, 60); }
    if (type === 'checkbox' || type === 'radio') item.checked = !!el.checked;
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') item.disabled = true;
    el.setAttribute('data-dsh-ix', String(item.i));
    items.push(item);
  }
  const active = document.activeElement && document.activeElement.getAttribute('data-dsh-ix');
  return { title: document.title, url: location.href, scrollY: Math.round(scrollY),
    scrollHeight: document.documentElement.scrollHeight, width: W, height: H, items, focused: active };
})()"#;

/// Readable text of the page (main content if there is one).
pub(crate) const TEXT: &str = r#"(() => {
  const root = document.querySelector('main, article, [role=main]') || document.body;
  return { title: document.title, url: location.href, text: (root ? root.innerText : '').replace(/\n{3,}/g, '\n\n').trim() };
})()"#;

/// Scrolls element `[index]` into the middle and returns its center.
pub(crate) fn center(index: &str) -> String {
    format!(
        r#"(() => {{
      const el = document.querySelector('[data-dsh-ix="{index}"]');
      if (!el) return null;
      el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
      const r = el.getBoundingClientRect();
      return {{ x: r.left + r.width / 2, y: r.top + r.height / 2, password: el.type === 'password',
        tag: el.tagName.toLowerCase(), editable: el.isContentEditable }};
    }})()"#
    )
}

/// Short name of element `[index]` for the mini window and the transcript row.
pub(crate) fn element_name(index: &str) -> String {
    format!(
        r#"(() => {{ const el = document.querySelector('[data-dsh-ix="{index}"]');
    return el ? (el.getAttribute('aria-label') || el.innerText || el.value || el.getAttribute('placeholder') || '').replace(/\s+/g, ' ').trim().slice(0, 60) : ''; }})()"#
    )
}

/// Picks an option of `<select>` `[index]` by value or label.
pub(crate) fn select(index: &str, value_json: &str) -> String {
    format!(
        r#"(() => {{
        const el = document.querySelector('[data-dsh-ix="{index}"]');
        if (!el || el.tagName !== 'SELECT') return 'not a <select>';
        const want = {value_json}.toLowerCase();
        const opt = [...el.options].find((o) => o.value.toLowerCase() === want || o.label.trim().toLowerCase() === want)
          || [...el.options].find((o) => o.label.toLowerCase().includes(want));
        if (!opt) return 'no option matches; options: ' + [...el.options].map((o) => o.label.trim()).slice(0, 30).join(' | ');
        el.value = opt.value;
        el.dispatchEvent(new Event('input', {{ bubbles: true }}));
        el.dispatchEvent(new Event('change', {{ bubbles: true }}));
        return true; }})()"#
    )
}

/// Selects the focused field's content before typing over it ("clear": true).
pub(crate) const SELECT_ACTIVE: &str = r#"(() => { const el = document.activeElement; if (!el) return;
          if (typeof el.select === 'function') el.select(); else document.execCommand('selectAll'); })()"#;

pub(crate) const ACTIVE_IS_PASSWORD: &str =
    r#"document.activeElement && document.activeElement.type === "password""#;
