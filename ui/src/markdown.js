// Markdown for assistant answers: GitHub flavour, raw HTML shown as text,
// only http(s)/mailto links, no remote images (nothing loads from the net).
import { Marked } from 'marked';
import { highlight, esc } from './highlight.js';
import { t } from './i18n.js';

const safeHref = (href) => (/^(https?:|mailto:|#)/i.test(href ?? '') ? href : null);

const md = new Marked({
  gfm: true,
  breaks: false,
  renderer: {
    html({ text }) {
      return esc(text);
    },
    link({ href, title, tokens }) {
      const label = this.parser.parseInline(tokens);
      const h = safeHref(href);
      if (!h) return label;
      return `<a href="${esc(h)}" target="_blank" rel="noopener noreferrer"${title ? ` title="${esc(title)}"` : ''}>${label}</a>`;
    },
    image({ href, text }) {
      return `<span class="md-img">${esc(text || href || '')}</span>`;
    },
    code({ text, lang }) {
      const name = (lang || '').trim().split(/\s+/)[0];
      return `<div class="md-code"><div class="md-code-head"><span>${esc(name || 'text')}</span>`
        + `<button type="button" class="md-copy" data-copy>${esc(t.copy)}</button></div>`
        + `<pre><code>${highlight(text, name)}</code></pre></div>`;
    },
    table(token) {
      // Same as marked's default, wrapped so wide tables scroll on their own.
      let head = '';
      for (const cell of token.header) head += this.tablecell(cell);
      let body = '';
      for (const row of token.rows) {
        let cells = '';
        for (const cell of row) cells += this.tablecell(cell);
        body += `<tr>${cells}</tr>`;
      }
      return `<div class="md-table"><table><thead><tr>${head}</tr></thead>${body ? `<tbody>${body}</tbody>` : ''}</table></div>`;
    },
  },
});

export function renderMarkdown(src) {
  try {
    return md.parse(src ?? '');
  } catch {
    return `<p>${esc(src ?? '')}</p>`;
  }
}

/** Delegated click handler for the copy buttons inside rendered Markdown. */
export function onMarkdownClick(ev) {
  const btn = ev.target.closest?.('[data-copy]');
  if (!btn) return;
  const code = btn.closest('.md-code')?.querySelector('pre')?.textContent ?? '';
  copyText(code).then(() => {
    btn.textContent = t.copied;
    setTimeout(() => { btn.textContent = t.copy; }, 1400);
  });
}

export function copyText(text) {
  if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
  return Promise.resolve(fallbackCopy(text));
}

function fallbackCopy(text) {
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.cssText = 'position:fixed;opacity:0';
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand('copy'); } catch { /* ignore */ }
  ta.remove();
}
