// A deliberately small syntax highlighter: comments, strings, numbers,
// keywords, types and calls for the languages a coding agent shows most.
// Output is escaped HTML with <span class="hl-*"> around tokens.

const esc = (s) => s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);

const words = (s) => new Set(s.split(' '));
const KEYWORDS = {
  js: words('async await break case catch class const continue debugger default delete do else export extends false finally for from function if import in instanceof interface let new null of return static super switch this throw true try type typeof undefined var void while with yield as enum implements readonly declare'),
  py: words('and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return self True try while with yield match case'),
  rs: words('as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while Some None Ok Err'),
  go: words('break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var nil true false'),
  sh: words('if then else elif fi for while until do done case esac in function return export local readonly echo cd exit set unset source sudo'),
  c: words('auto break case char const continue default do double else enum extern float for goto if int long register return short signed sizeof static struct switch typedef union unsigned void volatile while class public private protected new delete this true false null nullptr let var func guard import package fun val override'),
  json: words('true false null'),
};
const ALIAS = {
  js: 'js', javascript: 'js', mjs: 'js', cjs: 'js', jsx: 'js', ts: 'js', typescript: 'js', tsx: 'js',
  py: 'py', python: 'py', rs: 'rs', rust: 'rs', go: 'go', golang: 'go',
  sh: 'sh', bash: 'sh', shell: 'sh', zsh: 'sh', console: 'sh',
  json: 'json', jsonc: 'json', c: 'c', h: 'c', cpp: 'c', 'c++': 'c', java: 'c', swift: 'c', kotlin: 'c', kt: 'c', cs: 'c', csharp: 'c',
  toml: 'sh', yaml: 'sh', yml: 'sh',
};

const HASH_COMMENT = new Set(['py', 'sh']);

function pattern(lang) {
  const comment = HASH_COMMENT.has(lang) ? String.raw`#[^\n]*` : String.raw`\/\/[^\n]*|\/\*[\s\S]*?\*\/`;
  const strings = lang === 'rs'
    ? String.raw`"(?:\\.|[^"\\])*"`
    : String.raw`"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|\`(?:\\.|[^\`\\])*\``;
  return new RegExp(`(${comment})|(${strings})|(\\b\\d[\\d_]*(?:\\.\\d+)?(?:e[+-]?\\d+)?\\b|\\b0x[\\da-f]+\\b)|([A-Za-z_$][\\w$]*)(\\s*\\()?`, 'gi');
}
const CACHE = {};

export function highlight(code, langName) {
  const lang = ALIAS[(langName || '').toLowerCase()];
  if (!lang) return esc(code);
  const kw = KEYWORDS[lang];
  const re = (CACHE[lang] ??= pattern(lang));
  re.lastIndex = 0;
  let out = '';
  let last = 0;
  for (let m; (m = re.exec(code));) {
    out += esc(code.slice(last, m.index));
    last = re.lastIndex;
    const [all, comment, str, num, word, call] = m;
    if (comment) out += `<span class="hl-c">${esc(comment)}</span>`;
    else if (str) out += `<span class="hl-s">${esc(str)}</span>`;
    else if (num) out += `<span class="hl-n">${esc(num)}</span>`;
    else if (word) {
      let w;
      if (kw.has(word)) w = `<span class="hl-k">${esc(word)}</span>`;
      else if (call && lang !== 'json') w = `<span class="hl-f">${esc(word)}</span>`;
      else if (/^[A-Z][a-z0-9]/.test(word) && lang !== 'sh') w = `<span class="hl-t">${esc(word)}</span>`;
      else w = esc(word);
      out += w + (call ? esc(call) : '');
    } else out += esc(all);
  }
  return out + esc(code.slice(last));
}

export { esc };
