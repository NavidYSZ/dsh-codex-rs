// Settings → Aether app: the listener for the phone (switch, port, the
// addresses it is on, the certificate's fingerprint), pairing with a QR code
// the server renders, and the paired devices with rename and revoke. A
// server behind SSH (a VPS, an SSH instance of the Mac app) leads with the
// `dsh-codex pair --ssh` command, filled in with what the server and the Mac
// app know about it.
import { useEffect, useRef, useState } from 'preact/hooks';
import { html, cx } from '../html.js';
import { t, age } from '../i18n.js';
import { useStore } from '../store.js';
import { store, api, ask, snack, toast, fail, isMacPlatform } from '../state.js';
import { Field } from './settings.js';
import { Toggle, useLoad } from './page.js';
import { IconCopy } from '../icons.js';

const svgUrl = (svg) => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
const shortFingerprint = (hex) => (hex ? `${hex.slice(0, 8)} ${hex.slice(8, 16)} … ${hex.slice(-8)}` : '');

/** Where the Mac app keeps its `dsh-codex` when the bridge does not say. */
const BUNDLED_CLI = '/Applications/Aether.app/Contents/MacOS/dsh-codex';
/** `dsh-codex pair --ssh` defaults; flags equal to them are left out. */
const PAIR_DEFAULTS = { port: 3080, bin: '/usr/local/bin/dsh-codex' };

/** A word for a POSIX shell; `<placeholder>`s stay bare so the shell refuses them. */
const shellWord = (value) => (/^[\w@%+=:,./-]+$/.test(value) ? value : `'${value.replace(/'/g, `'\\''`)}'`);
const arg = (value, placeholder) => (value ? shellWord(String(value)) : `<${placeholder}>`);

/** The pairing command for this server, run on the Mac; `placeholders` = something is left to fill in. */
export function sshPairCommand({ remote, desktop, serverName }) {
  const r = remote ?? {};
  const words = [
    shellWord(desktop?.cliPath || BUNDLED_CLI), 'pair',
    '--ssh', arg(desktop?.ssh?.host, 'ssh-host'),
    '-F', arg(desktop?.ssh?.config, 'ssh_config'),
    ...(serverName ? ['--name', shellWord(serverName)] : []),
    '--remote-user', arg(r.user, 'remote-user'),
    '--remote-home', arg(r.home, 'remote-home'),
    '--unit', arg(r.unit, 'systemd-unit'),
    ...(r.port && r.port !== PAIR_DEFAULTS.port ? ['--remote-port', String(r.port)] : []),
    ...(r.bin && r.bin !== PAIR_DEFAULTS.bin ? ['--remote-bin', shellWord(r.bin)] : []),
  ];
  const text = words.join(' ');
  return { text, placeholders: /<[a-z_-]+>/.test(text) };
}

/** One paired device: name (inline rename), last seen, revoke. */
function DeviceRow({ client, onChange }) {
  const [editing, setEditing] = useState(null);
  const field = useRef(null);
  const renaming = editing != null;
  // The field appears after the click: focus it once it is there.
  useEffect(() => {
    if (!renaming) return;
    field.current?.focus();
    field.current?.select();
  }, [renaming]);
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
      ${!renaming ? html`<div class="field-label">${client.name}${client.current ? html` <span class="pill pill-ok">${t.mobileThisDevice}</span>` : null}</div>`
    : html`<form class="key-row" onSubmit=${(e) => { e.preventDefault(); save(); }}>
          <input class="input" ref=${field} value=${editing} maxlength="80" aria-label=${t.mobileRenameLabel}
            onInput=${(e) => setEditing(e.currentTarget.value)} onKeyDown=${(e) => { if (e.key === 'Escape') setEditing(null); }} />
          <button type="submit" class="btn btn-small btn-primary">${t.save}</button>
          <button type="button" class="btn btn-small" onClick=${() => setEditing(null)}>${t.cancel}</button>
        </form>`}
      <div class="field-hint">${[seen, t.mobilePairedVia[client.pairedVia] ?? client.pairedVia].join(' · ')}</div>
    </div>
    ${!renaming ? html`<div class="field-control key-row">
      <button type="button" class="btn btn-small" onClick=${() => setEditing(client.name)}>${t.rename}</button>
      <button type="button" class="btn btn-small" onClick=${revoke}>${t.mobileRevoke}</button>
    </div>` : null}
  </div>`;
}

/** The SSH pairing card: the command for this server, copyable. */
function SshPairing({ data, desktop }) {
  const command = sshPairCommand({ remote: data.sshPair, desktop, serverName: data.server?.name });
  const copy = async () => {
    try { await navigator.clipboard.writeText(command.text); snack(t.mobileCommandCopied); } catch { toast(t.mobileCommandCopyFailed); }
  };
  return html`<section class="card">
    <div class="card-title">${t.mobileSshTitle}</div>
    <p class="mb-note">${t.mobileSshHint}</p>
    <pre class="mb-cmd"><code>${command.text}</code></pre>
    ${command.placeholders ? html`<p class="mb-note mb-warn">${t.mobileSshPlaceholders}</p>` : null}
    <button type="button" class="btn btn-small" onClick=${copy}><${IconCopy} size=${13} /> ${t.mobileCopyCommand}</button>
  </section>`;
}

export function MobilePane() {
  const info = useStore(store, (s) => s.info);
  const desktopState = useStore(store, (s) => s.desktop);
  const { data, loading, error, reload } = useLoad(() => api.mobile(), []);
  const [busy, setBusy] = useState(false);
  const [portText, setPortText] = useState('');
  const [clientName, setClientName] = useState(t.mobileDefaultClient);
  // The last pairing made here: shown once, never stored.
  const [pairing, setPairing] = useState(null);
  // The saved port (0 = any free one), not the one the listener got.
  const configuredPort = data ? data.configuredPort ?? data.port : null;
  useEffect(() => { if (data) setPortText(String(configuredPort)); }, [configuredPort]);

  // A code that was never scanned goes, rather than staying behind in the list.
  const dropPairing = async () => {
    if (!pairing) return;
    setPairing(null);
    const previous = (data?.clients ?? []).find((c) => c.id === pairing.client.id);
    if (previous && !previous.lastSeenAt) await api.revokeMobileClient(previous.id).catch(() => {});
  };
  const refreshInfo = async () => { try { store.set({ info: await api.info() }); } catch { /* the page shows the old values */ } };
  const update = async (patch) => {
    setBusy(true);
    try {
      await api.updateMobile(patch);
      // The addresses or the port changed: a code shown now would point elsewhere.
      await dropPairing();
      reload();
      refreshInfo();
    } catch (e) { fail(e); } finally { setBusy(false); }
  };
  const makePair = async () => {
    setBusy(true);
    try {
      await dropPairing();
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
  // The Mac app's own SSH instance, else a server that is not a Mac (a VPS).
  const bridge = window.dshxDesktop;
  const instance = desktopState?.instances?.find((i) => i.id === desktopState.current);
  const behindSsh = instance?.kind === 'ssh' || (!!info?.platform && !isMacPlatform(info.platform));
  const desktop = bridge ? { cliPath: bridge.cliPath, ssh: instance?.kind === 'ssh' ? bridge.ssh : null } : null;
  const noAddress = pairing && !(pairing.payload?.transports ?? []).length;

  const listener = html`<section class="card">
      <${Field} label=${t.mobileReachable} hint=${t.mobileReachableHint}>
        <${Toggle} on=${data.enabled} disabled=${busy} label=${t.mobileReachable} onChange=${(on) => update({ enabled: on })} />
      </${Field}>
      <${Field} label=${t.mobilePort} hint=${listening && data.port !== configuredPort ? `${t.mobilePortHint} ${t.mobilePortActive(data.port)}` : t.mobilePortHint}>
        <form class="key-row" onSubmit=${(e) => { e.preventDefault(); if (portValid) update({ port: portValue }); }}>
          <input class="input mb-port" inputmode="numeric" value=${portText} aria-label=${t.mobilePort} onInput=${(e) => setPortText(e.currentTarget.value)} />
          <button type="submit" class="btn btn-small" disabled=${busy || !portValid || portValue === configuredPort}>${t.save}</button>
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
    </section>`;

  const qr = html`<section class="card">
      <div class="card-title">${behindSsh ? t.mobileConnectDirect : t.mobileConnect}</div>
      ${behindSsh ? html`<p class="mb-note">${t.mobileDirectNote}</p>` : null}
      <${Field} label=${t.mobileClientName} hint=${t.mobileClientNameHint}>
        <form class="key-row" onSubmit=${(e) => { e.preventDefault(); makePair(); }}>
          <input class="input" value=${clientName} maxlength="80" aria-label=${t.mobileClientName} onInput=${(e) => setClientName(e.currentTarget.value)} />
          <button type="submit" class=${cx('btn btn-small', !pairing && !behindSsh && 'btn-primary')} disabled=${busy}>${pairing ? t.mobileRegenerate : t.mobileMakeQr}</button>
        </form>
      </${Field}>
      ${!listening && !pairing ? html`<p class="mb-note mb-warn">${t.mobileListenerOffNote}</p>` : null}
      ${pairing ? html`<div class="mb-pair">
        <img class="mb-qr" alt=${t.mobileQrAlt} src=${svgUrl(pairing.qrSvg)} width="232" height="232" />
        <div class="mb-pair-side">
          ${noAddress ? html`<p class="mb-note mb-warn" role="alert">${behindSsh ? t.mobileNoTransportSsh : t.mobileNoTransport}</p>`
    : html`<p class="mb-note">${t.mobileScanHint}</p>`}
          <p class="mb-note mb-warn">${t.mobileSecretNote}</p>
          <button type="button" class="btn btn-small" onClick=${copyLink}><${IconCopy} size=${13} /> ${t.mobileCopyLink}</button>
        </div>
      </div>` : null}
    </section>`;

  return html`
    ${behindSsh ? html`<${SshPairing} data=${data} desktop=${desktop} />` : null}
    ${listener}
    ${qr}
    <section class="card">
      <div class="card-title">${t.mobileDevices}</div>
      ${clients.length ? clients.map((c) => html`<${DeviceRow} key=${c.id} client=${c} onChange=${reload} />`)
    : html`<p class="mb-empty">${t.mobileNoDevices}</p>`}
    </section>`;
}
