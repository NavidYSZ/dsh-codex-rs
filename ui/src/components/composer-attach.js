// Attachments of the next prompt: images go to the model as images (with
// dsh's limits and texts, `image.*`), every other file is stored by the
// server and referenced as `@path` (dsh file references), folders dropped
// from the project become `@folder/` references.
import { useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t } from '../i18n.js';
import { store, api } from '../state.js';
import { IconFile, IconX } from '../icons.js';
import { Thumb } from './common.js';
import { fileMention } from '../suggest.js';
import { notify } from './composer-extras.js';

const IMAGE_TYPES = /^image\/(png|jpe?g|webp|gif)$/;
/** dsh-attachment-local defaults (count, bytes per image, bytes per message). */
const MAX_IMAGES = 20;
const MAX_IMAGE_BYTES = 20 * 1024 * 1024;
const MAX_MESSAGE_IMAGE_BYTES = 200 * 1024 * 1024;
/** The server's limit for other files. */
const MAX_FILE_BYTES = 100 * 1024 * 1024;

const mb = (n) => `${Math.round(n / 1024 / 1024)} MB`;
const key = () => Math.random().toString(36).slice(2);

/**
 * Attachment state of one composer. `insert(text)` puts a reference at the
 * caret (dropped folders); `sel` is the composer's model selection.
 */
export function useAttachments({ sel, insert }) {
  const [items, setItems] = useState([]);
  const images = items.filter((i) => i.kind === 'image');
  const files = items.filter((i) => i.kind === 'file');

  const upload = async (item, file) => {
    setItems((xs) => [...xs, item]);
    try {
      const done = item.kind === 'image' ? { ref: await api.upload(file) } : { saved: await api.uploadFile(file) };
      setItems((xs) => xs.map((x) => (x.key === item.key ? { ...x, ...done } : x)));
    } catch (e) {
      setItems((xs) => xs.filter((x) => x.key !== item.key));
      notify(item.kind === 'image' ? t.imgSendFailed(e?.message || String(e)) : t.fileUploadFailed(item.name), 'error');
    }
  };

  /** Adds files from the picker, a paste or a drop. */
  const add = (list) => {
    const all = [...(list ?? [])];
    if (!all.length) return;
    let count = images.length;
    let bytes = images.reduce((n, i) => n + (i.bytes ?? 0), 0);
    const acceptsImages = sel?.modelInfo ? sel.modelInfo.inputModalities?.includes('image') : true;
    for (const f of all) {
      if (IMAGE_TYPES.test(f.type)) {
        if (!acceptsImages) { notify(t.imgModelUnsupported, 'error'); continue; }
        if (count >= MAX_IMAGES) { notify(t.imgTooMany(MAX_IMAGES), 'error'); break; }
        if (f.size > MAX_IMAGE_BYTES) { notify(t.imgTooLarge(mb(MAX_IMAGE_BYTES)), 'error'); continue; }
        if (bytes + f.size > MAX_MESSAGE_IMAGE_BYTES) { notify(t.imgTotalTooLarge(mb(MAX_MESSAGE_IMAGE_BYTES)), 'error'); break; }
        count += 1;
        bytes += f.size;
        upload({ key: key(), kind: 'image', name: f.name, bytes: f.size, ref: null }, f);
      } else {
        if (f.size > MAX_FILE_BYTES) { notify(t.fileTooLarge(mb(MAX_FILE_BYTES)), 'error'); continue; }
        upload({ key: key(), kind: 'file', name: f.name || 'file', bytes: f.size, saved: null }, f);
      }
    }
  };

  /**
   * A drop: folders become `@folder/` references (absolute paths from a
   * file manager's URI list, else a folder of that name in the project),
   * everything else is attached.
   */
  const drop = async (dt) => {
    if (!dt) return;
    const folders = [];
    const loose = [];
    const entries = [...(dt.items ?? [])].map((it) => (it.kind === 'file' ? it.webkitGetAsEntry?.() : null));
    [...(dt.files ?? [])].forEach((f, i) => {
      if (entries[i]?.isDirectory) folders.push(entries[i].name);
      else loose.push(f);
    });
    add(loose);
    if (!folders.length) return;
    const uris = (dt.getData?.('text/uri-list') ?? '').split(/\r?\n/).filter((u) => u.startsWith('file://'));
    for (const name of folders) {
      const uri = uris.find((u) => decodeURIComponent(u).replace(/\/$/, '').endsWith(`/${name}`));
      if (uri) { insert(`${fileMention(`${decodeURIComponent(new URL(uri).pathname).replace(/\/$/, '')}/`)} `); continue; }
      const found = await projectFolder(name);
      if (found) insert(`${found} `);
      else notify(t.folderNotInProject(name), 'error');
    }
  };

  const remove = (k) => setItems((xs) => xs.filter((x) => x.key !== k));
  const clear = () => setItems([]);
  return {
    items, images, files, add, drop, remove, clear,
    uploading: items.some((i) => !i.ref && !i.saved),
    /** Image refs for the prompt. */
    refs: () => images.map((i) => i.ref),
    /** The prompt text with a reference per uploaded file. */
    withFiles: (text) => {
      const refs = files.filter((f) => f.saved).map((f) => fileMention(f.saved.path));
      return refs.length ? `${text}${text ? '\n\n' : ''}${refs.join(' ')}` : text;
    },
  };
}

/** `@folder/` for a folder named `name` in the chat's project, if there is exactly one. */
async function projectFolder(name) {
  const s = store.get();
  try {
    const answer = await api.suggest({ kind: 'mention', q: name, ...(s.activeId ? { session: s.activeId } : { cwd: s.cwd }) });
    const files = answer?.sections?.find((x) => x.id === 'files')?.items ?? [];
    const hits = files.filter((f) => f.type === 'folder' && f.name === name);
    return hits.length === 1 ? hits[0].insert : null;
  } catch { return null; }
}

/** Images as thumbnails and files as chips above the input. */
export function AttachmentRow({ att }) {
  if (!att.items.length) return null;
  return html`<div class="composer-images">
    ${att.images.map((i) => html`<${Thumb} key=${i.key} image=${i.ref} label=${t.removeImage} onRemove=${() => att.remove(i.key)} />`)}
    ${att.files.map((f) => html`<span key=${f.key} class=${cx('file-chip', !f.saved && 'is-uploading')} title=${f.saved?.path ?? f.name}>
      <${IconFile} size=${14} /><span class="file-chip-name">${f.name}</span>
      <span class="file-chip-meta">${f.saved ? sizeLabel(f.bytes) : t.fileUploading}</span>
      <button type="button" class="file-chip-x" aria-label=${t.fileRemove(f.name)} onClick=${() => att.remove(f.key)}><${IconX} size=${11} /></button>
    </span>`)}
  </div>`;
}

function sizeLabel(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
