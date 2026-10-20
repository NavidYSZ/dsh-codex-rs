import { render } from 'preact';
import { html } from './html.js';
import { App } from './components/app.js';
import { boot, store, api } from './state.js';

render(html`<${App} />`, document.getElementById('app'));
boot();
// Handle for the dev screenshot script and the console in fixture mode.
if (api.fixture) window.dshx = { store, api };
