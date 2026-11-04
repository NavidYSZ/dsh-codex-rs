// Settings → Aether app: the listener for the phone (switch, port, the
// addresses it is on, the certificate's fingerprint), pairing with a QR code
// the server renders, and the paired devices with rename and revoke. A
// server behind SSH (a VPS) shows the `dsh-codex pair --ssh` command instead.
import { useEffect, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, age } from '../i18n.js';
import { useStore } from '../store.js';
import { store, api, ask, snack, toast, fail } from '../state.js';
import { Field } from './settings.js';
import { Toggle, useLoad } from './page.js';
import { IconCopy } from '../icons.js';

const svgUrl = (svg) => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
const shortFingerprint = (hex) => (hex ? `${hex.slice(0, 8)} ${hex.slice(8, 16)} … ${hex.slice(-8)}` : '');

/** One paired device: name (inline rename), last seen, revoke. */
function DeviceRow({ client, onChange }) {
  const [editing, setEditing] = useState(null);
  const save = async () => {
    const name = (editing ?? '').trim();
    setEditing(null);
    if (!name || name === client.name) return;
    try { await api.renameMobileClient(client.id, name); onChange(); } catch (e) { fail(e); }
  };
  const revoke = async () => {
    if (!(await ask({ title: t.mobileRevokeTitle(client.name), text: t.mobileRevokeText, action: t.mobileRevoke }))) return;
    try { await api.revokeMobileClient(client.id); onChange(); } catch (e) { fail(e); }
  };
  const seen = client.lastSeenAt ? t.mobileLastSeen(age(client.lastSeenAt)) : t.mobileNeverSeen;
  return html`<div class="field mb-device">
    <div class="field-text">
      ${editing == null ? html`<div class="field-label">${client.name}${client.current ? html` <span class="pill pill-ok">${t.mobileThisDevice}</span>` : null}</div>`
    : html`<form class="key-row" onSubmit=${(e) => { e.preventDefault(); save(); }}>
          <input class="input" value=${editing} maxlength="80" aria-label=${t.mobileRenameLabel} autofocus
            onInput=${(e) => setEditing(e.currentTarget.value)} onKeyDown=${(e) => { if (e.key === 'Escape') setEditing(null); }} />
          <button type="submit" class="btn btn-small btn-primary">${t.save}</button>
          <button type="button" class="btn btn-small" onClick=${() => setEditing(null)}>${t.cancel}</button>
        </form>`}
      <div class="field-hint">${[seen, t.mobilePairedVia[client.pairedVia] ?? client.pairedVia].join(' · ')}</div>
    </div>
    ${editing == null ? html`<div class="field-control key-row">
      <button type="button" class="btn btn-small" onClick=${() => setEditing(client.name)}>${t.rename}</button>
      <button type="button" class="btn btn-small" onClick=${revoke}>${t.mobileRevoke}</button>
    </div>` : null}
  </div>`;
}

export function MobilePane() {
  const info = useStore(store, (s) => s.info);
  const { data, loading, error, reload } = useLoad(() => api.mobile(), []);
  const [busy, setBusy] = useState(false);
  const [portText, setPortText] = useState('');
  const [clientName, setClientName] = useState(t.mobileDefaultClient);
  // The last pairing made here: shown once, never stored.
  const [pairing, setPairing] = useState(null);
  useEffect(() => { if (data) setPortText(String(data.port)); }, [data?.port]);

  const refreshInfo = async () => { try { store.set({ info: await api.info() }); } catch { /* the page shows the old values */ } };
  const update = async (patch) => {
    setBusy(true);
    try { await api.updateMobile(patch); reload(); refreshInfo(); } catch (e) { fail(e); } finally { setBusy(false); }
  };
  const makePair = async () => {
    setBusy(true);
    try {
      // A code that was never scanned is replaced, not left behind in the list.
      const previous = pairing && (data?.clients ?? []).find((c) => c.id === pairing.client.id);
      if (previous && !previous.lastSeenAt) await api.revokeMobileClient(previous.id).catch(() => {});
      setPairing(await api.pairMobile({ name: clientName.trim() || t.mobileDefaultClient }));
      reload();
    } catch (e) { fail(e); } finally { setBusy(false); }
  };
  const copyLink = async () => {
    try { await navigator.clipboard.writeText(pairing.link); snack(t.mobileLinkCopied); } catch { toast(t.mobileCopyFailed); }
  };

  if (error && error.status === 404) {
    return html`<div class="page-empty st-empty"><p>${t.mobileUnavailable}</p></div>`;
  }
  if (!data) return loading ? null : html`<div class="page-empty st-empty"><p>${t.mobileUnavailable}</p></div>`;
  const listening = !!data.certSha256;
  const addresses = (data.addresses ?? []).filter((a) => a.kind !== 'loopback');
  const clients = data.clients ?? [];
  const portValue = Number(portText);
  const portValid = portText !== '' && Number.isInteger(portValue) && portValue >= 0 && portValue <= 65535;
  const behindSsh = info?.platform && info.platform !== 'darwin';

  return html`<section class="card">
      <${Field} label=${t.mobileReachable} hint=${t.mobileReachableHint}>
        <${Toggle} on=${data.enabled} disabled=${busy} label=${t.mobileReachable} onChange=${(on) => update({ enabled: on })} />
      </${Field}>
      <${Field} label=${t.mobilePort} hint=${t.mobilePortHint}>
        <form class="key-row" onSubmit=${(e) => { e.preventDefault(); if (portValid) update({ port: portValue }); }}>
          <input class="input mb-port" inputmode="numeric" value=${portText} aria-label=${t.mobilePort} onInput=${(e) => setPortText(e.currentTarget.value)} />
          <button type="submit" class="btn btn-small" disabled=${busy || !portValid || portValue === data.port}>${t.save}</button>
        </form>
      </${Field}>
      <${Field} label=${t.mobileAddresses} hint=${listening ? t.mobileAddressesHint : t.mobileOffHint}>
        <div class="mb-chips">
          ${!listening ? html`<span class="field-value">${t.off}</span>`
    : addresses.length ? addresses.map((a) => html`<span class="pill mb-chip" key=${a.ip} title=${a.interface}><b>${t.mobileKinds[a.kind] ?? a.kind}</b>${a.ip}</span>`)
      : html`<span class="field-value">${t.mobileNoAddresses}</span>`}
        </div>
      </${Field}>
      ${listening ? html`<${Field} label=${t.mobileFingerprint} hint=${t.mobileFingerprintHint}>
        <span class="field-value mono" title=${data.certSha256}>${shortFingerprint(data.certSha256)}</span>
      </${Field}>` : null}
    </section>

    ${behindSsh ? html`<section class="card">
      <div class="card-title">${t.mobileSshTitle}</div>
      <p class="mb-note">${t.mobileSshHint}</p>
      <pre class="mb-cmd"><code>dsh-codex pair --ssh ${'<host>'} -F ${'<ssh_config>'} --name ${JSON.stringify(data.server?.name ?? '')}</code></pre>
    </section>` : null}

    <section class="card">
      <div class="card-title">${t.mobileConnect}</div>
      <${Field} label=${t.mobileClientName} hint=${t.mobileClientNameHint}>
        <form class="key-row" onSubmit=${(e) => { e.preventDefault(); makePair(); }}>
          <input class="input" value=${clientName} maxlength="80" aria-label=${t.mobileClientName} onInput=${(e) => setClientName(e.currentTarget.value)} />
          <button type="submit" class=${cx('btn btn-small', !pairing && 'btn-primary')} disabled=${busy}>${pairing ? t.mobileRegenerate : t.mobileMakeQr}</button>
        </form>
      </${Field}>
      ${!listening && !behindSsh ? html`<p class="mb-note mb-warn">${t.mobileListenerOffNote}</p>` : null}
      ${pairing ? html`<div class="mb-pair">
        <img class="mb-qr" alt=${t.mobileQrAlt} src=${svgUrl(pairing.qrSvg)} width="232" height="232" />
        <div class="mb-pair-side">
          <p class="mb-note">${t.mobileScanHint}</p>
          <p class="mb-note mb-warn">${t.mobileSecretNote}</p>
          <button type="button" class="btn btn-small" onClick=${copyLink}><${IconCopy} size=${13} /> ${t.mobileCopyLink}</button>
        </div>
      </div>` : null}
    </section>

    <section class="card">
      <div class="card-title">${t.mobileDevices}</div>
      ${clients.length ? clients.map((c) => html`<${DeviceRow} key=${c.id} client=${c} onChange=${reload} />`)
    : html`<p class="mb-empty">${t.mobileNoDevices}</p>`}
    </section>`;
}
